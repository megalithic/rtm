use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rtm::{
    model::{self, Inputs, Model},
    pick::{
        actions::{self, Io, Tab},
        parse_key,
        state::{self, App, Command, ItemKind, Mode},
    },
};
use std::cell::RefCell;

fn model() -> Model {
    #[derive(serde::Deserialize)]
    struct Fixture {
        now: u64,
        registry: rtm::registry::Registry,
        worktrees: rtm::git::Worktrees,
        prs: rtm::git::PullRequests,
        seen: rtm::seen::Seen,
    }
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/enrichment.json")).unwrap();
    let rex: Vec<rtm::rex::Session> =
        serde_json::from_str(include_str!("fixtures/rex.json")).unwrap();
    let ultimates: Vec<rtm::ultimate::Instance> =
        serde_json::from_str(include_str!("fixtures/ultimate.json")).unwrap();
    model::build(&Inputs {
        rex: &rex,
        ultimates: &ultimates,
        registry: &fixture.registry,
        worktrees: &fixture.worktrees,
        prs: &fixture.prs,
        seen: &fixture.seen,
        now: fixture.now,
    })
}

fn key(name: &str) -> KeyEvent {
    parse_key(name).unwrap_or_else(|| panic!("key {name}"))
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn selected_name(app: &App) -> String {
    match &app.selected_item().unwrap().kind {
        ItemKind::Project { name, .. } => format!("[{name}]"),
        _ => app.current().unwrap().0.name.clone(),
    }
}

fn select_named(app: &mut App, name: &str) {
    for _ in 0..app.items.len() {
        if selected_name(app) == name {
            return;
        }
        app.key(key("ctrl+n"));
    }
    panic!("no row {name}");
}

#[test]
fn default_list_groups_attention_and_children() {
    let app = App::new(model());
    let kinds: Vec<_> = app.items.iter().map(|item| item.kind.clone()).collect();
    assert!(matches!(
        kinds[0],
        ItemKind::Header {
            group: 0,
            attention: 1
        }
    ));
    assert!(matches!(kinds[1], ItemKind::Row { group: 0, row: 0 }));
    assert!(matches!(
        kinds[2],
        ItemKind::Child {
            group: 0,
            row: 0,
            child: 0
        }
    ));
    // Attention rows end in a quiet gap before the rest of the group.
    let rest = kinds
        .iter()
        .position(|kind| matches!(kind, ItemKind::Row { group: 0, row: 1 }))
        .unwrap();
    assert_eq!(kinds[rest - 1], ItemKind::Gap);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, ItemKind::Header { group: 1, .. }))
    );
    // The first selectable row is highlighted: the instance that needs you.
    assert_eq!(selected_name(&app), "atlas");
}

fn select_project(app: &mut App, name: &str) {
    let index = app
        .items
        .iter()
        .position(|item| matches!(&item.kind, ItemKind::Project { name: n, .. } if n == name))
        .unwrap_or_else(|| panic!("no project {name}"));
    app.select(index);
}

#[test]
fn enter_on_a_project_header_goes_to_its_main_session_or_opens_one() {
    // atlas' main checkout has its own session (atlas-main): Enter jumps there.
    let mut app = App::new(model());
    select_project(&mut app, "atlas");
    assert_eq!(selected_name(&app), "[atlas]");
    match app.key(key("enter")) {
        Command::Jump { row, seen } => {
            assert_eq!(row.name, "atlas-main");
            assert_eq!(seen, std::slice::from_ref(&row.key));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        app.tell_target().unwrap().args(),
        ["show", "tell", "--project", "/Users/demo/code/atlas"]
    );
    // Without it (only another session in the main checkout, as one hosting an Ultimate,
    // and worktree sessions), Enter opens the main checkout's own session.
    let mut model = model();
    for row in model.groups.iter_mut().flat_map(|group| &mut group.rows) {
        if row.name == "atlas-main" {
            row.name = "atlas ultimate".into();
        }
    }
    let mut app = App::new(model);
    select_project(&mut app, "atlas");
    match app.key(key("enter")) {
        Command::OpenMain { path, name } => {
            assert_eq!(path, "/Users/demo/code/atlas");
            assert_eq!(name, "atlas");
        }
        other => panic!("{other:?}"),
    }
    // rwt's labels for main checkouts, and the arguments for each kind of project.
    assert_eq!(state::main_label("/Users/seth/.dotfiles"), "dotfiles-main");
    assert_eq!(
        state::main_label("/Users/seth/code/work/strive/provider_portal"),
        "provider_portal-main"
    );
    assert_eq!(
        actions::open_main_args("/p/provider_portal", true),
        ["open", "/p/provider_portal", "--json", "--show"]
    );
    assert_eq!(
        actions::open_main_args("/p/.dotfiles", false),
        [
            "open",
            "/p/.dotfiles",
            "--json",
            "--show",
            "--no-services",
            "--no-agent"
        ]
    );
}

#[test]
fn ctrl_n_p_and_arrows_skip_headers_and_stop_at_ends() {
    let mut app = App::new(model());
    app.key(key("ctrl+p"));
    assert_eq!(selected_name(&app), "atlas");
    app.key(key("ctrl+n"));
    assert_eq!(selected_name(&app), "Database indexes");
    app.key(key("down"));
    app.key(key("up"));
    assert_eq!(selected_name(&app), "Database indexes");
    for _ in 0..50 {
        app.key(key("down"));
    }
    let last = app.selected;
    app.key(key("ctrl+n"));
    assert_eq!(app.selected, last);
    assert!(app.selected_item().unwrap().selectable());
}

#[test]
fn typing_searches_and_esc_clears_before_closing() {
    let mut app = App::new(model());
    typed(&mut app, "indexes");
    assert_eq!(app.query, "indexes");
    // A sub-row match keeps its parent visible, right above it.
    let child = app
        .items
        .iter()
        .position(|item| matches!(item.kind, ItemKind::Child { .. }))
        .unwrap();
    assert!(matches!(app.items[child - 1].kind, ItemKind::Row { .. }));
    assert_eq!(
        app.row(&app.items[child]).unwrap().0.name,
        "Database indexes"
    );
    app.key(key("backspace"));
    assert_eq!(app.query, "indexe");
    assert!(matches!(app.key(key("esc")), Command::None));
    assert!(app.query.is_empty());
    assert!(matches!(app.items[0].kind, ItemKind::Header { .. }));
    assert!(matches!(app.key(key("esc")), Command::Quit));
    typed(&mut app, "zzzzzzzz");
    assert!(app.items.is_empty());
    assert!(matches!(app.key(key("enter")), Command::None));
}

#[test]
fn enter_jumps_and_marks_the_row_and_its_parent_seen() {
    let mut app = App::new(model());
    app.key(key("ctrl+n"));
    match app.key(key("enter")) {
        Command::Jump { row, seen } => {
            assert_eq!(row.name, "Database indexes");
            assert_eq!(
                seen,
                ["ult:/Users/demo/code/atlas#2", "ult:/Users/demo/code/atlas"]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn kill_confirms_with_y_and_cancels_on_anything_else() {
    let mut app = App::new(model());
    app.key(key("ctrl+k"));
    assert_eq!(app.mode, Mode::List); // Ultimate rows cannot be killed.
    assert!(app.note.is_some());
    select_named(&mut app, "atlas-search");
    app.key(key("ctrl+k"));
    assert!(matches!(&app.mode, Mode::Kill { name, .. } if name == "atlas-search"));
    assert!(matches!(app.key(key("n")), Command::None));
    assert_eq!(app.mode, Mode::List);
    assert!(app.query.is_empty()); // The cancelling key is not typed into search.
    app.key(key("ctrl+k"));
    match app.key(key("y")) {
        Command::Kill { session, name } => {
            assert_eq!(session, "session:10");
            assert_eq!(name, "atlas-search");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(app.mode, Mode::List);
    assert_eq!(
        actions::kill_args("session:10"),
        ["down", "--session", "session:10", "--json"]
    );
}

#[test]
fn new_worktree_input_with_and_without_base() {
    let mut app = App::new(model());
    select_named(&mut app, "atlas-search");
    app.key(key("ctrl+t"));
    assert!(
        matches!(&app.mode, Mode::Create { base: None, repo, path, .. } if repo == "atlas" && path.ends_with("/.worktrees/search"))
    );
    assert!(matches!(app.key(key("enter")), Command::None)); // Empty name: nothing to run.
    typed(&mut app, "fix-thingx");
    app.key(key("backspace"));
    match app.key(key("enter")) {
        Command::Create { name, base, .. } => {
            assert_eq!(name, "fix-thing");
            assert_eq!(base, None);
        }
        other => panic!("{other:?}"),
    }
    // Keys wait while rwt runs; a failure keeps the input and shows why.
    typed(&mut app, "ignored");
    app.created(Err("name taken".into()));
    assert!(
        matches!(&app.mode, Mode::Create { input, error: Some(error), busy: false, .. } if input == "fix-thing" && error == "name taken")
    );
    app.key(key("esc"));
    assert_eq!(app.mode, Mode::List);
    assert!(app.query.is_empty());

    app.key(key("ctrl+shift+t"));
    assert!(matches!(&app.mode, Mode::Create { base: Some(base), .. } if base == "search"));
    app.key(key("ctrl+shift+t")); // Toggles back to the default branch, and again.
    assert!(matches!(&app.mode, Mode::Create { base: None, .. }));
    app.key(key("ctrl+shift+t"));
    typed(&mut app, "child");
    assert!(
        matches!(app.key(key("enter")), Command::Create { base: Some(base), name, .. } if base == "search" && name == "child")
    );
    app.created(Ok(()));
    assert_eq!(app.mode, Mode::List);
    assert_eq!(
        actions::create_args("child", Some("search")),
        ["new", "child", "--json", "--show", "--base", "search"]
    );
}

#[test]
fn refresh_keeps_the_selection_by_key() {
    let mut app = App::new(model());
    select_named(&mut app, "atlas-search");
    let mut next = model();
    next.groups[1].rows.reverse();
    app.set_model(next);
    assert_eq!(selected_name(&app), "atlas-search");
}

#[test]
fn ctrl_o_opens_tidewave_for_the_rows_app() {
    let mut app = App::new(model());
    select_named(&mut app, "atlas-search");
    assert!(
        matches!(app.key(key("ctrl+o")), Command::Tidewave(url) if url == "http://search.local/tidewave/connect")
    );
}

#[test]
fn tab_tells_the_rows_project_ultimate() {
    use rtm::model::Kind;
    use rtm::pick::state::Tell;
    let mut model = model();
    // A Rex session outside any repository has no project to tell.
    let rex = model
        .groups
        .iter_mut()
        .find(|group| group.kind == Kind::Rex)
        .unwrap();
    let mut loose = rex.rows[0].clone();
    loose.key = "rex:loose".into();
    loose.name = "scratch".into();
    loose.metadata.repo = None;
    loose.children.clear();
    rex.rows.push(loose);
    // An agent sub-row carries no repo of its own: it tells its session's project.
    let session = rex
        .rows
        .iter_mut()
        .find(|row| row.name == "atlas-search")
        .unwrap();
    let mut agent = session.clone();
    agent.key = format!("{}/block", session.key);
    agent.kind = Kind::Agent;
    agent.name = "claude".into();
    agent.metadata = Default::default();
    agent.children.clear();
    session.children.push(agent);
    let inbox_url = model.groups[0].rows[0].metadata.origin.clone().unwrap();
    model.groups[0].rows[0].children.insert(
        0,
        rtm::model::Row {
            key: "ult:inbox".into(),
            kind: Kind::Inbox,
            name: "Inbox".into(),
            state: "idle".into(),
            attention: false,
            last_viewed: 0,
            last_activity: 1,
            metadata: Default::default(),
            jump: rtm::model::JumpTarget::Inbox {
                url: inbox_url,
                key: "message:9".into(),
            },
            preview: None,
            details: Vec::new(),
            children: Vec::new(),
        },
    );
    let mut app = App::new(model);
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..app.items.len() {
        if !app.items[index].selectable() {
            continue;
        }
        app.select(index);
        let Some((row, parent)) = app.current() else {
            continue;
        };
        let owner = parent.unwrap_or(row);
        let expected = match owner.kind {
            Kind::Ultimate => Some(Tell::Url(owner.metadata.origin.clone().unwrap())),
            _ => owner.metadata.repo.clone().map(Tell::Project),
        };
        assert_eq!(app.tell_target(), expected, "{}", row.name);
        seen.insert(format!("{:?}/{}", row.kind, expected.is_some()));
        match app.key(key("tab")) {
            Command::Tell(target) => assert_eq!(Some(target), expected),
            Command::None => assert!(expected.is_none()),
            other => panic!("{other:?}"),
        }
    }
    // Every row kind the fixture has was checked: instances, their conversations, Rex
    // sessions and their agents, and the loose session without a project.
    for kind in [
        "Ultimate/true",
        "Inbox/true",
        "Conversation/true",
        "Rex/true",
        "Agent/true",
        "Rex/false",
    ] {
        assert!(seen.contains(kind), "{kind} not covered: {seen:?}");
    }
    let first = app.items.iter().position(|item| item.selectable()).unwrap();
    app.select(first);
    assert_eq!(
        app.tell_target().unwrap().args(),
        ["show", "tell", "--url", "http://127.0.0.1:4700"]
    );
    select_named(&mut app, "atlas-search");
    assert_eq!(
        app.tell_target().unwrap().args(),
        ["show", "tell", "--project", "/Users/demo/code/atlas"]
    );
}

#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    tabs: Option<&'static str>,
    fail_ui: bool,
}

impl Io for Fake {
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String> {
        self.calls.borrow_mut().push(format!(
            "{method} {url} {} {}",
            headers
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(","),
            body.unwrap_or_default()
        ));
        if url.ends_with("/api/ui") && self.fail_ui {
            bail!("refused");
        }
        if url.ends_with("/json/list") {
            return self
                .tabs
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("CDP down"));
        }
        Ok(String::new())
    }

    fn run(&self, program: &str, args: &[&str], _cwd: Option<&str>) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(format!("run {program} {}", args.join(" ")));
        Ok(String::new())
    }
}

const TABS: &str = r#"[
  {"id":"W","type":"service_worker","url":"http://127.0.0.1:4700/sw.js"},
  {"id":"X","type":"page","url":"http://127.0.0.1:47001/"},
  {"id":"A","type":"page","url":"http://localhost:4700/?conversation=3"},
  {"id":"B","type":"page","url":"http://127.0.0.1:4700/"}
]"#;

#[test]
fn ultimate_jump_posts_the_ui_state_then_raises_its_tab() {
    unsafe { std::env::remove_var("PIDEWAVE_CDP_PORT") };
    let fake = Fake {
        tabs: Some(TABS),
        ..Fake::default()
    };
    actions::jump_ultimate(&fake, "http://127.0.0.1:4700", 7).unwrap();
    let calls = fake.calls.borrow();
    assert_eq!(
        calls[0],
        r#"POST http://127.0.0.1:4700/api/ui Origin=http://127.0.0.1:4700,Content-Type=application/json {"set":{"shell.conversation":7}}"#
    );
    assert_eq!(calls[1], "GET http://127.0.0.1:9223/json/list  ");
    assert_eq!(calls[2], "GET http://127.0.0.1:9223/json/activate/A  ");
    assert_eq!(calls[3], "run open -a Helium");
    assert_eq!(calls.len(), 4);
}

#[test]
fn ultimate_jump_opens_the_link_without_cdp_or_ui_state() {
    let fake = Fake::default();
    actions::jump_ultimate(&fake, "http://127.0.0.1:4700", 7).unwrap();
    assert_eq!(
        fake.calls.borrow().last().unwrap(),
        "run open http://127.0.0.1:4700/?conversation=7"
    );
    let fake = Fake {
        tabs: Some(TABS),
        fail_ui: true,
        ..Fake::default()
    };
    actions::jump_ultimate(&fake, "http://127.0.0.1:4700", 7).unwrap();
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1], "run open http://127.0.0.1:4700/?conversation=7");
}

#[test]
fn cdp_tab_matching_needs_a_page_on_the_same_origin() {
    let tabs: Vec<Tab> = serde_json::from_str(TABS).unwrap();
    assert_eq!(
        actions::find_tab(&tabs, "http://127.0.0.1:4700")
            .unwrap()
            .id,
        "A"
    );
    assert_eq!(
        actions::find_tab(&tabs, "http://127.0.0.1:4700/")
            .unwrap()
            .id,
        "A"
    );
    assert!(actions::find_tab(&tabs, "http://127.0.0.1:470").is_none()); // Port prefix is not a match.
    assert!(actions::find_tab(&tabs, "http://127.0.0.1:4701").is_none());
    let tidewave: Vec<Tab> = serde_json::from_str(
        r#"[{"id":"T","type":"page","url":"http://localhost:4044/tidewave/connect"}]"#,
    )
    .unwrap();
    assert_eq!(
        actions::find_tab(&tidewave, "http://localhost:4044/tidewave")
            .unwrap()
            .id,
        "T"
    );
}

#[test]
fn rex_jump_focuses_the_block_then_selects_with_the_client() {
    let fake = Fake::default();
    actions::jump_rex(
        &fake,
        Some("client:1"),
        "session:3",
        Some("window:4"),
        Some("block:2"),
    )
    .unwrap();
    let calls = fake.calls.borrow();
    assert!(calls[0].ends_with(
        r#"--autostart=false api call session.focus_block {"block_id":"block:2","session_id":"session:3"}"#
    ));
    assert!(calls[1].ends_with(
        "--autostart=false -C client:1 do session.select session_id=session:3 window_id=window:4"
    ));
    assert_eq!(calls.len(), 2);
}

#[test]
fn keys_parse_including_ctrl_shift() {
    let event = key("ctrl+shift+t");
    assert_eq!(event.code, KeyCode::Char('t'));
    assert_eq!(event.modifiers, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(parse_key("hyper+x").is_none());
}

#[test]
fn an_in_flight_preview_does_not_wake_the_loop() {
    use rtm::pick::next_wake;
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let past = now - Duration::from_secs(1);
    // Expired but blocked behind an in-flight request: sleep until input or the response.
    assert_eq!(next_wake(Some(past), true, None, None), None);
    assert_eq!(next_wake(Some(past), false, None, None), Some(past));
    let frame = now + Duration::from_millis(16);
    assert_eq!(next_wake(Some(past), true, Some(frame), None), Some(frame));
    let spin = now + Duration::from_millis(100);
    assert_eq!(next_wake(None, false, Some(frame), Some(spin)), Some(frame));
    assert_eq!(next_wake(None, false, None, None), None);
}

#[test]
fn a_stuck_capture_is_killed_after_its_limit() {
    use std::time::{Duration, Instant};
    let mut command = std::process::Command::new("sleep");
    command.arg("5");
    let start = Instant::now();
    let error = rtm::pick::preview::output_within(command, Duration::from_millis(200)).unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(error.to_string().contains("timed out"));
    let mut command = std::process::Command::new("echo");
    command.arg("hi");
    let output = rtm::pick::preview::output_within(command, Duration::from_secs(2)).unwrap();
    assert_eq!(output.stdout, b"hi\n");
}

#[test]
fn rex_sessions_group_under_their_project_with_ports_in_one_column() {
    use ratatui::{buffer::Buffer, layout::Rect};
    use rtm::pick::ui;
    use std::time::Instant;
    let mut model = model();
    // A PR without a port, and a five-digit port: the cases that misaligned.
    let rex = model
        .groups
        .iter_mut()
        .find(|group| group.rows.iter().any(|row| row.name == "atlas-search"))
        .unwrap();
    let mut docs = rex
        .rows
        .iter()
        .find(|row| row.name == "atlas-search")
        .unwrap()
        .clone();
    docs.key = "rex:docs".into();
    docs.name = "atlas-docs".into();
    docs.metadata.apps.clear();
    let mut wide = docs.clone();
    wide.key = "rex:wide".into();
    wide.name = "atlas-wide".into();
    wide.metadata.pr = None;
    wide.metadata.apps = vec![rtm::model::Endpoint {
        port: 10000,
        origin: None,
    }];
    rex.rows.push(docs);
    rex.rows.push(wide);
    let app = App::new(model);
    let area = Rect::new(0, 0, 70, 30);
    let mut buf = Buffer::empty(area);
    ui::draw(
        &mut buf,
        &app,
        &mut ui::View::default(),
        Instant::now(),
        1_794_925_000_000,
    );
    let lines: Vec<String> = (0..30)
        .map(|y| (0..70).map(|x| buf[(x, y)].symbol().to_owned()).collect())
        .collect();
    let screen = lines.join("\n");
    let rex = lines
        .iter()
        .position(|line| line.trim().starts_with("Rex"))
        .unwrap();
    let lines = &lines[rex..];
    let line = |needle: &str| {
        lines
            .iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in\n{screen}"))
            .clone()
    };
    // One header per project; its sessions beneath it without the `atlas-` prefix.
    assert_eq!(
        lines.iter().filter(|line| line.trim() == "atlas").count(),
        1,
        "{screen}"
    );
    assert!(
        !screen.contains("atlas-search") && !screen.contains("atlas-docs"),
        "{screen}"
    );
    let header = line("atlas").find("atlas").unwrap();
    let search = line(" search ");
    assert!(search.find("search").unwrap() > header, "{screen}");
    // PR first, then the port: every port ends in one column, and a PR without a port
    // stays in the PR column.
    // Columns, not bytes: the status glyphs differ in UTF-8 length.
    let end =
        |line: &str, token: &str| line[..line.find(token).unwrap()].chars().count() + token.len();
    let main = line(":4000");
    assert_eq!(end(&search, ":4001"), end(&main, ":4000"), "{screen}");
    assert_eq!(
        end(&line(" wide "), ":10000"),
        end(&main, ":4000"),
        "{screen}"
    );
    assert!(end(&search, "#42") < end(&search, ":4001"), "{screen}");
    assert_eq!(end(&line(" docs "), "#42"), end(&search, "#42"), "{screen}");
}

#[test]
fn tiny_buffers_render_without_panicking() {
    use ratatui::{buffer::Buffer, layout::Rect};
    use rtm::pick::{draw_kill, ui};
    use std::time::Instant;
    for (width, height) in [
        (1, 1),
        (5, 2),
        (20, 2),
        (76, 7),
        (4, 1),
        (30, 3),
        (31, 8),
        (100, 9),
        (200, 60),
    ] {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        draw_kill(
            &mut buf,
            area,
            "provider_portal-sm-admin-auto-enroll-provider",
        );
        // Offset areas too: Rex draws the layer at its own origin.
        let shifted = Rect::new(3, 2, width, height);
        let mut offset = Buffer::empty(shifted);
        draw_kill(&mut offset, shifted, "x");
        let mut app = App::new(model());
        for mode in [
            "",
            "ctrl+n ctrl+t",
            "ctrl+n ctrl+n ctrl+n ctrl+n ctrl+n ctrl+n ctrl+n ctrl+n ctrl+k",
        ] {
            for name in mode.split_whitespace() {
                app.key(key(name));
            }
            let mut view = ui::View::default();
            let mut buf = Buffer::empty(area);
            ui::draw(&mut buf, &app, &mut view, Instant::now(), 1_794_925_000_000);
            app.key(key("esc"));
        }
    }
    let area = Rect::new(0, 0, 76, 7);
    let mut buf = Buffer::empty(area);
    draw_kill(&mut buf, area, "box");
    let text: String = (0..7)
        .map(|y| {
            (0..76)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(
        text.contains("kill box?") && text.contains("any key cancel"),
        "{text}"
    );
    let area = Rect::new(0, 0, 40, 2);
    let mut buf = Buffer::empty(area);
    draw_kill(&mut buf, area, "box");
    let row = |y| {
        (0..40)
            .map(|x| buf[(x, y)].symbol().to_owned())
            .collect::<String>()
    };
    assert!(row(0).contains("kill box?"));
    assert!(row(1).contains("y kill"));
    // A long name is shortened in the middle; the question mark survives.
    let mut buf = Buffer::empty(area);
    draw_kill(
        &mut buf,
        area,
        "provider_portal-sm-admin-auto-enroll-provider",
    );
    let row = (0..40)
        .map(|x| buf[(x, 0)].symbol().to_owned())
        .collect::<String>();
    assert!(
        row.contains("…") && row.trim_end().ends_with("provider?"),
        "{row}"
    );
}

#[test]
fn ctrl_shift_t_on_an_ultimate_row_reads_the_branch_from_its_checkout() {
    fn fake_branch(path: &str) -> Option<String> {
        (path == "/Users/demo/code/atlas/.worktrees/db").then(|| "db-indexes".to_owned())
    }
    let mut app = App::new(model());
    app.branch_of = fake_branch;
    // "Database indexes" is a conversation whose cwd is the db worktree; it has no branch metadata.
    select_named(&mut app, "Database indexes");
    assert!(app.current().unwrap().0.metadata.branch.is_none());
    app.key(key("ctrl+shift+t"));
    assert!(
        matches!(&app.mode, Mode::Create { base: Some(base), path, .. } if base == "db-indexes" && path.ends_with("/.worktrees/db"))
    );
    app.key(key("esc"));
    // No branch anywhere (the instance's project here): a quiet note, no input.
    let mut app = App::new(model());
    app.branch_of = fake_branch;
    assert_eq!(selected_name(&app), "atlas");
    app.key(key("ctrl+shift+t"));
    assert_eq!(app.mode, Mode::List);
    assert!(app.note.is_some());
}

fn has_rgb(lines: &[ratatui::text::Line]) -> bool {
    use ratatui::style::Color;
    lines.iter().flat_map(|line| &line.spans).any(|span| {
        matches!(span.style.fg, Some(Color::Rgb(..)))
            || matches!(span.style.bg, Some(Color::Rgb(..)))
    })
}

#[test]
fn markdown_code_uses_the_terminal_palette() {
    use ratatui::style::Color;
    use rtm::pick::markdown;
    let lines =
        markdown::render("Call `answer()` first.\n\n```rust\nfn answer() -> u32 { 42 }\n```\n");
    assert!(!has_rgb(&lines), "{lines:?}");
    let span = |text: &str| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(text))
            .unwrap_or_else(|| panic!("no {text} in {lines:?}"))
            .clone()
    };
    // Inline code and the fenced block's keyword, type and number each carry a palette index.
    assert!(matches!(span("answer()").style.fg, Some(Color::Indexed(_))));
    for token in ["fn", "u32", "42"] {
        assert!(
            matches!(span(token).style.fg, Some(Color::Indexed(_))),
            "{token}: {lines:?}"
        );
    }
    assert_ne!(span("fn").style.fg, span("42").style.fg);
    // The fence itself is not shown.
    assert!(!lines.iter().any(|line| line.to_string().contains("```")));
    // Shell commands in tool lines highlight the same way.
    let shell = markdown::shell("mix test --max-failures 1 | tail -5");
    assert!(
        shell
            .iter()
            .any(|span| matches!(span.style.fg, Some(Color::Indexed(_))))
    );
}

#[test]
fn conversation_preview_has_markdown_tools_and_no_truecolor() {
    use ratatui::style::Color;
    use rtm::pick::preview::conversation_text;
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/conversation.json")).unwrap();
    let text = conversation_text(&entries);
    assert!(!has_rgb(&text.lines));
    let rendered: Vec<String> = text.lines.iter().map(ToString::to_string).collect();
    assert_eq!(rendered[0], "you");
    let edit = text
        .lines
        .iter()
        .find(|line| line.to_string().contains("▸ edit"))
        .unwrap();
    assert!(
        edit.spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Indexed(4))
                && span.content.contains("router.ex"))
    );
    // Wrapping keeps each span's colour.
    let wrapped = rtm::pick::ui::wrap(&text, 40);
    for line in &wrapped {
        let text = line.to_string();
        // Tool lines stay whole (one row each, clipped when drawn); everything else fits.
        assert!(
            text.starts_with("  ▸ ") || line.width() <= 40,
            "{} {text:?}",
            line.width()
        );
    }
    let tools = wrapped
        .iter()
        .filter(|line| line.to_string().starts_with("  ▸ "))
        .count();
    assert_eq!(tools, 3);
    assert!(!has_rgb(&wrapped));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Fixed the login redirect"))
    );
}

/// `cargo test --release --test pick timing -- --ignored --nocapture`
#[test]
#[ignore]
fn timing_markdown_preview() {
    use std::time::Instant;
    let start = Instant::now();
    rtm::pick::markdown::warm();
    let load = start.elapsed();
    let one: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/conversation.json")).unwrap();
    let entries: Vec<serde_json::Value> = one.iter().cycle().take(40).cloned().collect();
    let start = Instant::now();
    std::hint::black_box(rtm::pick::preview::conversation_text(&entries));
    let first = start.elapsed();
    println!("first (cold) 40-entry render: {first:?}");
    let runs = 50;
    let start = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(rtm::pick::preview::conversation_text(&entries));
    }
    let render = start.elapsed() / runs;
    println!("syntax+theme load: {load:?}; 40-entry render: {render:?}");
}

#[test]
fn inbox_jump_sets_the_surface_then_raises_its_tab() {
    let fake = Fake {
        tabs: Some(TABS),
        ..Fake::default()
    };
    actions::jump_inbox(&fake, "http://127.0.0.1:4700", "message:5").unwrap();
    let calls = fake.calls.borrow();
    assert_eq!(
        calls[0],
        r#"POST http://127.0.0.1:4700/api/ui Origin=http://127.0.0.1:4700,Content-Type=application/json {"set":{"shell.surface":"inbox"}}"#
    );
    assert!(calls[1].ends_with("/json/list  "));
    assert!(calls[2].ends_with("/json/activate/A  "));
    assert_eq!(calls[3], "run open -a Helium");
    // No tab: the surface is still set, then a new tab opens on the oldest entry.
    let fake = Fake::default();
    actions::jump_inbox(&fake, "http://127.0.0.1:4700", "inbox-items:3").unwrap();
    let calls = fake.calls.borrow();
    assert!(calls[0].contains(r#"{"set":{"shell.surface":"inbox"}}"#));
    assert_eq!(
        calls.last().unwrap(),
        "run open http://127.0.0.1:4700/?inbox=inbox-items:3"
    );
}

#[test]
fn inbox_preview_lists_entries_with_kind_and_amber_waits() {
    use ratatui::style::Color;
    use rtm::{pick::preview::inbox_text, ultimate::InboxEntry};
    let entry = |key: &str, kind: &str, title: &str, at: u64, detail: Option<&str>| InboxEntry {
        key: key.into(),
        kind: kind.into(),
        title: title.into(),
        detail: detail.map(Into::into),
        at,
    };
    let now = 10 * 3_600_000;
    let text = inbox_text(
        &[
            entry(
                "message:1",
                "question",
                "Ship it?",
                now - 2 * 3_600_000,
                Some("Which branch?"),
            ),
            entry("inbox-items:2", "link", "Preview", now - 60_000, None),
        ],
        now,
    );
    let lines: Vec<String> = text.lines.iter().map(ToString::to_string).collect();
    assert_eq!(lines[0], "question  Ship it?  2h");
    assert_eq!(lines[1], "          Which branch?");
    assert_eq!(lines[2], "link      Preview  1m");
    let age = |line: usize| text.lines[line].spans.last().unwrap().style.fg;
    assert_eq!(age(0), Some(Color::Indexed(3))); // Over an hour: amber.
    assert_eq!(age(2), Some(Color::Indexed(8)));
}

#[test]
fn enter_on_the_inbox_row_jumps_to_the_inbox() {
    let mut model = model();
    model.groups[0].rows[0].children.insert(
        0,
        rtm::model::Row {
            key: "ult:/Users/demo/code/atlas/inbox".into(),
            kind: rtm::model::Kind::Inbox,
            name: "Inbox".into(),
            state: "blocked".into(),
            attention: true,
            last_viewed: 0,
            last_activity: 1,
            metadata: Default::default(),
            jump: rtm::model::JumpTarget::Inbox {
                url: "http://127.0.0.1:4700".into(),
                key: "message:9".into(),
            },
            preview: None,
            details: vec!["inbox".into()],
            children: Vec::new(),
        },
    );
    let mut app = App::new(model);
    app.key(key("ctrl+n"));
    assert_eq!(selected_name(&app), "Inbox");
    match app.key(key("enter")) {
        Command::Jump { row, seen } => {
            assert!(matches!(row.jump, rtm::model::JumpTarget::Inbox { .. }));
            assert_eq!(seen[0], "ult:/Users/demo/code/atlas/inbox");
        }
        other => panic!("{other:?}"),
    }
}

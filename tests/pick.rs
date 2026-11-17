use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rtm::{
    model::{self, Inputs, Model},
    pick::{
        actions::{self, Io, Tab},
        parse_key,
        state::{App, Command, ItemKind, Mode},
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
    app.current().unwrap().0.name.clone()
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

#[test]
fn ctrl_n_p_and_arrows_skip_headers_and_stop_at_ends() {
    let mut app = App::new(model());
    app.key(key("ctrl+p"));
    assert_eq!(selected_name(&app), "atlas");
    app.key(key("ctrl+n"));
    assert_eq!(selected_name(&app), "migration");
    app.key(key("down"));
    app.key(key("up"));
    assert_eq!(selected_name(&app), "migration");
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
    assert_eq!(app.row(&app.items[child]).unwrap().0.name, "migration");
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
            assert_eq!(row.name, "migration");
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

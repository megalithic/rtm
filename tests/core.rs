use rtm::{
    Timestamp,
    git::{Colour, PullRequest, PullRequests, Worktrees},
    model::{self, Inputs, JumpTarget, Kind, Model, PreviewSource},
    registry::Registry,
    rex::{Session, Status},
    search,
    seen::{Seen, Store},
    ultimate::Instance,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Fixture {
    now: u64,
    registry: Registry,
    worktrees: Worktrees,
    prs: PullRequests,
    seen: Seen,
    #[serde(skip)]
    rex: Vec<Session>,
    #[serde(skip)]
    ultimates: Vec<Instance>,
}

impl Fixture {
    fn load() -> Self {
        let mut fixture: Self =
            serde_json::from_str(include_str!("fixtures/enrichment.json")).unwrap();
        fixture.rex = serde_json::from_str(include_str!("fixtures/rex.json")).unwrap();
        fixture.ultimates = serde_json::from_str(include_str!("fixtures/ultimate.json")).unwrap();
        fixture
    }

    fn model(&self) -> Model {
        model::build(&Inputs {
            rex: &self.rex,
            ultimates: &self.ultimates,
            registry: &self.registry,
            worktrees: &self.worktrees,
            prs: &self.prs,
            seen: &self.seen,
            now: self.now,
        })
    }
}

#[test]
fn live_schema_empty_lua_lists_and_conversation_parents() {
    let sessions = rtm::rex::decode(include_bytes!("fixtures/rex-empty.json")).unwrap();
    assert!(sessions[0].blocks[0].records().is_empty());
    assert!(rtm::rex::decode(b"{}").unwrap().is_empty());
    assert!(rtm::rex::decode(br#"{"unexpected":1}"#).is_err());
    let fixture = Fixture::load();
    let parent = fixture.ultimates[0].conversations[1]
        .parent
        .as_ref()
        .unwrap();
    assert_eq!(parent.conversation_id, 1);
    assert_eq!(parent.at, 100);
}

#[test]
fn groups_folds_hosts_retains_mixed_sessions_and_locations() {
    let mut fixture = Fixture::load();
    let model = fixture.model();
    assert_eq!(
        model
            .groups
            .iter()
            .map(|group| group.kind)
            .collect::<Vec<_>>(),
        [Kind::Ultimate, Kind::Rex]
    );
    assert_eq!(model.groups[0].rows.len(), 2);
    assert_eq!(model.groups[1].rows.len(), 2);
    let ultimate = &model.groups[0].rows[0];
    assert_eq!(ultimate.name, "atlas");
    assert!(
        matches!(&ultimate.jump, JumpTarget::Ultimate { rex: Some(location), .. }
        if location.session == "session:3" && location.window.as_deref() == Some("window:6") && location.block == "block:1")
    );
    let rex = &model.groups[1].rows[0];
    assert_eq!(rex.name, "atlas-main");
    assert_eq!(rex.state, "done"); // Folded blocked host must not leak into the Rex row.
    assert_eq!(rex.metadata.agent_count, 1);
    assert!(rex.children.is_empty());
    assert!(matches!(&rex.preview, Some(PreviewSource::Rex { block }) if block == "block:2"));
    assert!(
        matches!(&rex.jump, JumpTarget::Rex { window: Some(window), .. } if window == "window:4")
    );
    fixture.rex[0].blocks.truncate(1);
    assert_eq!(fixture.model().groups[1].rows.len(), 1); // Ultimate-only session disappears.
    fixture.ultimates[0].pid = 9999;
    assert_eq!(fixture.model().groups[1].rows.len(), 2); // App name alone is not enough to fold.
}

#[test]
fn ultimate_children_attention_recency_seen_and_root_name() {
    let fixture = Fixture::load();
    let model = fixture.model();
    let row = &model.groups[0].rows[0];
    assert!(row.attention);
    assert_eq!(row.state, "blocked");
    assert_eq!(row.metadata.agent_count, 4);
    assert_eq!(
        row.children
            .iter()
            .map(|child| child.name.as_str())
            .collect::<Vec<_>>(),
        // Idle conversations ("Recent review", "@ultimate") fold into "+2 more".
        ["Database indexes", "Background worker"]
    );
    assert_eq!(row.metadata.hidden, 2);
    assert!(row.children[0].attention);
    assert_eq!(row.children[0].last_viewed, 1794923000000);
    assert_eq!(row.last_viewed, 1794924500000); // Includes concluded conversations for last-viewed.
    assert!(model.groups[0].rows[1].children.is_empty());
    assert!(row.last_activity >= 1794924500000);
    let mut failed = Fixture::load();
    failed.ultimates[0].status.as_mut().unwrap().questions.count = 0;
    failed.ultimates[0].status.as_mut().unwrap().failures = vec![json!({"conversation":3})];
    assert!(
        failed.model().groups[0]
            .rows
            .iter()
            .find(|row| row.name == "atlas")
            .unwrap()
            .attention
    );
}

#[test]
fn rex_unread_done_blocked_children_and_subrow_order() {
    let mut fixture = Fixture::load();
    let done = fixture.rex[0].blocks[1].records()[0].activity();
    assert_eq!(done, 1794860265882); // Offset + fractional RFC3339 is epoch milliseconds.
    fixture.seen.insert("rex:session:3".into(), done);
    let model = fixture.model();
    assert!(
        !model.groups[1]
            .rows
            .iter()
            .find(|row| row.name == "atlas-main")
            .unwrap()
            .attention
    );
    let mut second = fixture.rex[0].blocks[1].clone();
    second.id = "block:extra".into();
    let record = &mut second.status.as_mut().unwrap().records[0];
    record.state = "blocked".into();
    record.msg = Some("Need credentials".into());
    record.updated_at = Some(Timestamp::Millis(1));
    fixture.rex[0].blocks.push(second);
    let model = fixture.model();
    let row = model.groups[1]
        .rows
        .iter()
        .find(|row| row.name == "atlas-main")
        .unwrap();
    assert!(row.attention);
    assert_eq!(row.children.len(), 2);
    assert_eq!(row.children[0].name, "Need credentials");
    assert!(row.children[0].attention); // Blocked remains attention even if already seen.
    assert!(!row.children[1].attention);
    fixture.seen.insert("rex:session:3".into(), done - 1);
    assert!(fixture.model().groups[1].rows[0].children[1].attention);
}

#[test]
fn rows_sort_attention_then_viewed_then_activity_then_name() {
    let mut fixture = Fixture::load();
    fixture.ultimates.clear();
    fixture.rex[0].blocks[0].status = Some(Status::default());
    fixture.rex[1].blocks[0].status = Some(Status::default());
    // Unread done precedes a more recently viewed idle row.
    fixture.seen.insert("rex:session:10".into(), fixture.now);
    assert_eq!(fixture.model().groups[1].rows[0].name, "atlas-main");
    fixture.seen.insert("rex:session:3".into(), fixture.now + 1);
    assert_eq!(fixture.model().groups[1].rows[0].name, "atlas-main");
    fixture.seen.insert("rex:session:3".into(), fixture.now);
    assert_eq!(fixture.model().groups[1].rows[0].name, "atlas-main"); // Newer activity breaks equal viewed.
    fixture.rex[1].blocks[1].status = fixture.rex[0].blocks[1].status.clone();
    fixture.rex[1].label = "aaa".into();
    assert_eq!(fixture.model().groups[1].rows[0].name, "aaa");
}

#[test]
fn worktree_branch_pr_and_deepest_app_match() {
    let fixture = Fixture::load();
    let model = fixture.model();
    let row = model.groups[1]
        .rows
        .iter()
        .find(|row| row.name == "atlas-search")
        .unwrap();
    assert_eq!(row.metadata.branch.as_deref(), Some("search"));
    assert_eq!(
        row.metadata.worktree.as_deref(),
        Some("/Users/demo/code/atlas/.worktrees/search")
    );
    let pr = row.metadata.pr.as_ref().unwrap();
    assert_eq!(pr.number, 42);
    assert_eq!(pr.colour, Colour::Green);
    assert_eq!(row.metadata.apps.len(), 1);
    assert_eq!(row.metadata.apps[0].port, 4001);
    assert_eq!(
        row.metadata.apps[0].origin.as_deref(),
        Some("http://search.local")
    );
}

#[test]
fn search_tiers_child_hits_and_highlight_indices() {
    let mut fixture = Fixture::load();
    // One genuine name match must beat a newer, attention-bearing conversation title hit.
    fixture.rex[1].label = "indexes".into();
    fixture
        .seen
        .insert("ult:/Users/demo/code/atlas".into(), fixture.now);
    let model = fixture.model();
    let hits = search::search(&model, "indexes");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].tier, 1);
    assert_eq!(hits[0].group, 1);
    assert_eq!(hits[0].matches[0].indices, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(hits[1].tier, 2);
    assert_eq!(hits[1].group, 0);
    assert_eq!(hits[1].children.len(), 1);
    let child = &model.groups[hits[1].group].rows[hits[1].row].children[hits[1].children[0].child];
    assert_eq!(child.name, "Database indexes");
    let found = &hits[1].children[0].matches[0];
    let matched: String = found
        .indices
        .iter()
        .map(|index| found.text.chars().nth(*index as usize).unwrap())
        .collect();
    assert_eq!(matched.to_lowercase(), "indexes");
    assert_eq!(search::search(&model, "").len(), 4);
    assert!(search::search(&model, "qqqqqqqqqqqq").is_empty());
    // A single agent has no sub-row but its status message is still tier 2 searchable.
    assert!(
        search::search(&model, "Waiting")
            .iter()
            .any(|hit| hit.group == 1 && hit.tier == 2)
    );
}

#[test]
fn search_within_tier_viewed_precedes_attention_then_score() {
    let mut model = Fixture::load().model();
    let rows = &mut model.groups[1].rows;
    rows[0].name = "alpha".into();
    rows[1].name = "alpha beta".into();
    rows[0].last_viewed = 10;
    rows[1].last_viewed = 20;
    rows[0].attention = true;
    rows[1].attention = false;
    assert_eq!(search::search(&model, "alpha")[0].row, 1);
    model.groups[1].rows[0].last_viewed = 20;
    assert_eq!(search::search(&model, "alpha")[0].row, 0);
    model.groups[1].rows[0].attention = false;
    let hits = search::search(&model, "alpha");
    assert!(hits[0].score >= hits[1].score);
}

#[test]
fn pr_colours_cover_conflicts_drafts_and_all_failure_states() {
    let fixture = Fixture::load();
    let base = fixture.prs.values().next().unwrap()[0].clone();
    assert_eq!(base.colour(), Colour::Green);
    let mut pr = base.clone();
    pr.is_draft = true;
    assert_eq!(pr.colour(), Colour::Blue);
    pr.mergeable = "CONFLICTING".into();
    assert_eq!(pr.colour(), Colour::Red);
    pr = base.clone();
    pr.merge_state_status = "DIRTY".into();
    assert_eq!(pr.colour(), Colour::Red);
    for failure in [
        "FAILURE",
        "ERROR",
        "CANCELLED",
        "TIMED_OUT",
        "ACTION_REQUIRED",
        "STARTUP_FAILURE",
    ] {
        for field in ["state", "conclusion"] {
            pr = base.clone();
            pr.status_check_rollup = Some(vec![json!({field:failure})]);
            assert_eq!(pr.colour(), Colour::Red, "{field}={failure}");
        }
    }
    let pending: PullRequest = serde_json::from_value(json!({"number":1,"headRefName":"topic","mergeStateStatus":"BLOCKED","statusCheckRollup":null,"url":"https://example.test/pr/1"})).unwrap();
    assert_eq!(pending.colour(), Colour::Blue);
}

/// The same colours as `rwt pr` (dotfiles 0fce2290): yellow while checks run, green only once
/// the review is approved or not required.
#[test]
fn pr_colours_match_rwt() {
    let fixture = Fixture::load();
    let base = fixture.prs.values().next().unwrap()[0].clone();
    let mut pr = base.clone();
    pr.status_check_rollup = Some(vec![
        json!({"__typename":"CheckRun","status":"IN_PROGRESS"}),
    ]);
    assert_eq!(pr.colour(), Colour::Yellow);
    pr.status_check_rollup = Some(vec![
        json!({"__typename":"StatusContext","state":"PENDING"}),
    ]);
    assert_eq!(pr.colour(), Colour::Yellow);
    // A failure outranks a running check.
    pr.status_check_rollup
        .as_mut()
        .unwrap()
        .push(json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}));
    assert_eq!(pr.colour(), Colour::Red);
    pr = base.clone();
    pr.review_decision = Some("REVIEW_REQUIRED".into());
    assert_eq!(pr.colour(), Colour::Blue);
    pr.review_decision = Some("APPROVED".into());
    assert_eq!(pr.colour(), Colour::Green);
    pr.merge_state_status = "HAS_HOOKS".into();
    assert_eq!(pr.colour(), Colour::Green);
}

#[test]
fn seen_round_trip_merges_writers_and_rejects_corruption() {
    let dir = std::env::temp_dir().join(format!("rtm-seen-test-{}", std::process::id()));
    let path = dir.join("seen.json");
    let mut first = Store::open(&path).unwrap();
    let mut second = Store::open(&path).unwrap();
    assert_eq!(first.get("missing"), 0);
    first.mark_at("rex:session:3", 123).unwrap();
    second.mark_at("ult:/demo#1", 456).unwrap();
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.get("rex:session:3"), 123);
    assert_eq!(reopened.get("ult:/demo#1"), 456);
    reopened.mark_at("rex:session:3", 100).unwrap();
    assert_eq!(Store::open(&path).unwrap().get("rex:session:3"), 123);
    std::fs::write(&path, b"broken").unwrap();
    assert!(Store::open(&path).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ultimate_subrows_keep_recent_two_hours_and_at_most_five() {
    let mut fixture = Fixture::load();
    let now = fixture.now;
    let base = fixture.ultimates[0].conversations[0].clone();
    let extra = |id, busy, input_at| rtm::ultimate::Conversation {
        id,
        title: Some(format!("extra {id}")),
        busy,
        input_at,
        ..base.clone()
    };
    // Three hours old and idle: no longer a sub-row.
    fixture.ultimates[0]
        .conversations
        .push(extra(40, false, Some(now - 3 * 3_600_000)));
    let row = fixture.model().groups[0].rows[0].clone();
    assert_eq!(row.metadata.agent_count, 4);
    assert!(!row.children.iter().any(|child| child.name == "extra 40"));
    for id in 50..60 {
        fixture.ultimates[0]
            .conversations
            .push(extra(id, true, Some(now - 1000)));
    }
    let row = fixture.model().groups[0].rows[0].clone();
    assert_eq!(row.children.len(), 5);
    assert_eq!(row.metadata.agent_count, 14);
    assert_eq!(row.metadata.hidden, 9);
    assert_eq!(row.children[0].name, "Database indexes"); // Attention survives the cut.
}

#[test]
fn dead_phoenix_apps_are_hidden() {
    let mut fixture = Fixture::load();
    fixture.registry.apps[1].dead = true;
    let model = fixture.model();
    let row = model.groups[1]
        .rows
        .iter()
        .find(|row| row.name == "atlas-search")
        .unwrap();
    // The worktree's own app is dead; the parent checkout's app must not stand in.
    assert!(row.metadata.apps.is_empty());
    let dead: Registry = serde_json::from_value(
        json!({"apps":[{"port":4000,"origin":"http://a","root":"/r","dead":true}]}),
    )
    .unwrap();
    assert!(rtm::registry::apps_for(&dead, &["/r/x"]).is_empty());
}

fn messages_doc() -> serde_json::Value {
    json!({"next": 6, "messages": [
        {"id": 1, "from": 2, "to": "user", "text": "**Ship it?** The branch is green.", "at": 300, "reply": true, "state": "delivered"},
        {"id": 2, "from": 2, "to": "user", "text": "Old question", "at": 100, "reply": true, "state": "answered"},
        {"id": 3, "from": 2, "to": "user", "text": "FYI only", "at": 110, "reply": false, "state": "delivered"},
        {"id": 4, "from": 2, "to": 3, "text": "agent to agent", "at": 120, "reply": true, "state": "delivered"},
        {"id": 5, "from": 3, "to": "user", "text": "## Pick a name\nmore", "at": 50, "reply": true, "state": "delivered",
         "questions": [{"text": "Which name?", "kind": "choice"}]}
    ]})
}

fn items_doc() -> serde_json::Value {
    json!({"next": 4, "items": [
        {"id": 1, "kind": "artifact", "title": "Drawer part page", "text": "", "conversation": 48, "at": 200},
        {"id": 2, "kind": "action", "title": "Done already", "text": "", "conversation": 48, "at": 10, "done": 999},
        {"id": 3, "kind": "link", "title": "Preview", "text": "", "conversation": 48, "at": 400, "url": "http://localhost:4000"}
    ]})
}

#[test]
fn inbox_counts_open_questions_and_items_oldest_first() {
    use rtm::ultimate::{doc_result, inbox};
    let entries = inbox(Some(&messages_doc()), Some(&items_doc()));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        ["message:5", "inbox-items:1", "message:1", "inbox-items:3"]
    );
    assert_eq!(entries[0].title, "Pick a name");
    assert_eq!(entries[0].detail.as_deref(), Some("Which name?"));
    assert_eq!(entries[2].title, "Ship it?"); // Bold lead.
    assert_eq!(entries[1].kind, "artifact");
    // An instance without the inbox-items extension: questions only, not an error.
    let missing =
        doc_result(json!({"error": "Unknown conversation document: inbox-items.items"})).unwrap();
    assert!(missing.is_none());
    assert_eq!(inbox(Some(&messages_doc()), missing.as_ref()).len(), 2);
    assert!(doc_result(json!({"error": "boom"})).is_err());
    assert_eq!(
        doc_result(json!({"result": {"items": []}})).unwrap(),
        Some(json!({"items": []}))
    );
    assert!(inbox(None, None).is_empty());
}

#[test]
fn inbox_row_leads_subrows_and_only_questions_need_you() {
    use rtm::ultimate::inbox;
    let mut fixture = Fixture::load();
    // Items only: the Inbox shows, quietly; the instance's attention is unchanged.
    fixture.ultimates[1].inbox = inbox(None, Some(&items_doc()));
    let model = fixture.model();
    let quiet = model.groups[0]
        .rows
        .iter()
        .find(|row| row.key.ends_with("search"))
        .unwrap();
    assert!(!quiet.attention);
    let row = &quiet.children[0];
    assert_eq!((row.kind, row.name.as_str()), (Kind::Inbox, "Inbox"));
    assert_eq!(row.metadata.agent_count, 2);
    assert!(!row.attention);
    assert!(
        matches!(&row.jump, JumpTarget::Inbox { url, key } if url == "http://127.0.0.1:4701" && key == "inbox-items:1")
    );
    assert!(matches!(&row.preview, Some(PreviewSource::Inbox { entries }) if entries.len() == 2));
    // Searchable by "inbox" and by an item's title.
    for query in ["inbox", "Drawer part"] {
        assert!(
            search::search(&model, query)
                .iter()
                .any(|hit| hit.children.iter().any(|child| child.child == 0)),
            "{query}"
        );
    }
    // A question: the Inbox needs you. It leads, and the five conversation rows stay.
    let base = fixture.ultimates[0].conversations[0].clone();
    for id in 50..60 {
        fixture.ultimates[0]
            .conversations
            .push(rtm::ultimate::Conversation {
                id,
                busy: true,
                ..base.clone()
            });
    }
    fixture.ultimates[0].inbox = inbox(Some(&messages_doc()), None);
    let model = fixture.model();
    let atlas = &model.groups[0].rows[0];
    assert_eq!(atlas.children[0].kind, Kind::Inbox);
    assert!(atlas.children[0].attention);
    assert_eq!(atlas.children[0].state, "blocked");
    assert_eq!(atlas.children.len(), 6);
    assert_eq!(atlas.metadata.hidden, 9);
    // No open entries: no row.
    fixture.ultimates[0].inbox.clear();
    assert!(
        fixture.model().groups[0].rows[0]
            .children
            .iter()
            .all(|row| row.kind != Kind::Inbox)
    );
}

#[test]
fn needs_reads_other_hosts_remotes_and_an_unreachable_hub_has_no_count() {
    use rtm::needs;
    let settings = json!({"remotes": {
        "workbookpro": {".dotfiles": "https://workbookpro.ts.net:4749"},
        "megabookpro": {".dotfiles": "https://megabookpro.ts.net:4749", "odd": 3}
    }});
    assert_eq!(
        needs::remotes(&settings, "WorkbookPro"),
        [(
            "megabookpro".into(),
            ".dotfiles".into(),
            "https://megabookpro.ts.net:4749".into()
        )]
    );
    assert!(needs::remotes(&json!({}), "x").is_empty());
    // Port 9 (discard) refuses at once: an error and no count, never 0.
    let home = std::env::temp_dir().join(format!("rtm-needs-{}", std::process::id()));
    std::fs::create_dir_all(home.join("projects")).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"remotes": {"elsewhere": {"p": "http://127.0.0.1:9"}}}).to_string(),
    )
    .unwrap();
    let result = needs::load(&home).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert_eq!(result.total, 0);
    assert_eq!(result.hubs.len(), 1);
    assert_eq!(result.hubs[0].count, None);
    assert!(result.hubs[0].error.is_some());
}

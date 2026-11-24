//! An Ultimate hub's automations, for the menu bar: each one's state in the words of the hub's
//! Automations panel (`extensions/automations/parts.tsx` `lookOf`), and pause, resume or run by id.
//! Ids restart in every chapter, so every change also names the automation and is refused when the
//! hub's automation by that id has another name.
use crate::ultimate;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub pinned: bool,
    pub enabled: bool,
    /// `schedule`, `event`, `context` or `script`.
    pub kind: &'static str,
    /// The panel's word: failed, investigating, writing, working, watching, scheduled, armed,
    /// waiting, paused or done.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fire_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_at: Option<u64>,
    /// The script's error or the last failed run's answer, when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const OPERATIONS: [&str; 3] = ["pause", "resume", "run"];

/// The panel's `looks[…].rank`: worst first.
fn rank(state: &str) -> u8 {
    match state {
        "failed" => 0,
        "investigating" => 1,
        "writing" | "working" => 2,
        "watching" | "scheduled" | "armed" | "waiting" => 3,
        "paused" => 4,
        _ => 5,
    }
}

/// The panel's `lookOf` for one `automations/list` item.
pub fn state(item: &Value, now: u64) -> &'static str {
    let script = item.pointer("/script/state").and_then(Value::as_str);
    match script {
        Some("failed") => return "failed",
        Some("investigating") => return "investigating",
        Some("writing") => return "writing",
        _ => {}
    }
    let enabled = item["enabled"].as_bool().unwrap_or(false);
    let last = item["runs"]
        .as_array()
        .and_then(|runs| runs.last())
        .and_then(|run| run["status"].as_str());
    if last == Some("running") {
        return "working";
    }
    if last == Some("failed") && (enabled || script == Some("done")) {
        return "failed";
    }
    match script {
        Some("running") => return "watching",
        Some("done") => return "done",
        _ => {}
    }
    if enabled {
        if let Some(condition) = item.get("condition") {
            return if condition["armed"].as_bool().unwrap_or(false) {
                "armed"
            } else {
                "waiting"
            };
        }
        return if item.get("event").is_some() {
            "watching"
        } else {
            "scheduled"
        };
    }
    let spent = item.pointer("/schedule/kind").and_then(Value::as_str) == Some("at")
        && item
            .pointer("/schedule/time")
            .and_then(Value::as_str)
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .is_some_and(|time| time.timestamp_millis() as u64 <= now);
    if spent { "done" } else { "paused" }
}

fn automation(item: &Value, now: u64) -> Option<Automation> {
    let kind = if item.get("script").is_some() {
        "script"
    } else if item.get("condition").is_some() {
        "context"
    } else if item.get("event").is_some() {
        "event"
    } else {
        "schedule"
    };
    let state = state(item, now);
    let error = (state == "failed")
        .then(|| {
            item.pointer("/script/error")
                .and_then(Value::as_str)
                .or_else(|| item["runs"].as_array()?.last()?.get("answer")?.as_str())
        })
        .flatten()
        .map(str::to_owned);
    Some(Automation {
        id: item["id"].as_str()?.to_owned(),
        name: item["name"].as_str()?.to_owned(),
        pinned: item["pinned"].as_bool().unwrap_or(false),
        enabled: item["enabled"].as_bool().unwrap_or(false),
        kind,
        state,
        last_fire_at: item["lastFireAt"].as_u64(),
        next_at: item["nextAt"].as_u64(),
        error,
    })
}

/// `automations/list` as the menu shows it: pinned first, then worst first, then by name.
pub fn parse(list: &Value, now: u64) -> Vec<Automation> {
    let mut items: Vec<Automation> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| automation(item, now))
        .collect();
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(rank(a.state).cmp(&rank(b.state)))
            .then(a.name.cmp(&b.name))
    });
    items
}

/// The hub's automations; an instance without the automations extension has none.
pub fn read(url: &str, timeout: Duration) -> Result<Vec<Automation>> {
    match ultimate::action_within(url, "automations/actions/list", Value::Null, timeout)? {
        Some(list) => Ok(parse(&list, crate::now_ms())),
        None => Ok(Vec::new()),
    }
}

/// Pause, resume or run automation `id` on the hub, only while it is still called `name`.
pub fn act(url: &str, id: &str, name: &str, operation: &str) -> Result<Automation> {
    if !OPERATIONS.contains(&operation) {
        bail!("unknown operation {operation}: pause, resume or run");
    }
    let timeout = Duration::from_secs(5);
    let find = |list: &[Automation]| list.iter().find(|item| item.id == id).cloned();
    match find(&read(url, timeout)?) {
        None => bail!("{url} has no automation {id}"),
        Some(found) if found.name != name => bail!(
            "{url} automation {id} is now \"{}\", not \"{name}\"; refresh and try again",
            found.name
        ),
        Some(_) => {}
    }
    let path = format!("automations/actions/{operation}");
    if ultimate::action_within(url, &path, json!({"id": id}), timeout)?.is_none() {
        bail!("{url} has no automations");
    }
    find(&read(url, timeout)?).ok_or_else(|| anyhow::anyhow!("{url} automation {id} disappeared"))
}

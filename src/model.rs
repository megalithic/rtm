//! Pure projection: all filesystem, process, clock and network inputs are supplied by the caller.
use crate::{
    git::{Colour, PullRequests, Worktree, Worktrees},
    registry::{self, Registry},
    rex::{self, Block, Location, Record},
    seen::Seen,
    ultimate::{self, Conversation},
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Ultimate,
    Rex,
    Conversation,
    Agent,
    Inbox,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Model {
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Group {
    pub kind: Kind,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Row {
    pub key: String,
    pub kind: Kind,
    pub name: String,
    pub state: String,
    pub attention: bool,
    pub last_viewed: u64,
    pub last_activity: u64,
    pub metadata: Metadata,
    pub jump: JumpTarget,
    pub preview: Option<PreviewSource>,
    /// Secondary search text remains available even when a single agent has no visible sub-row.
    pub details: Vec<String>,
    pub children: Vec<Row>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Metadata {
    pub project: Option<String>,
    pub port: Option<u16>,
    pub origin: Option<String>,
    pub hub: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub pr: Option<Pr>,
    pub agent_count: usize,
    /// Relevant sub-rows left out after the first few; shown as "+N more".
    #[serde(default)]
    pub hidden: usize,
    pub apps: Vec<Endpoint>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Endpoint {
    pub port: u16,
    pub origin: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pr {
    pub number: u64,
    pub url: String,
    pub colour: Colour,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JumpTarget {
    Rex {
        session: String,
        window: Option<String>,
        block: Option<String>,
    },
    Ultimate {
        url: String,
        conversation: u64,
        rex: Option<Location>,
    },
    /// The instance's Inbox; `key` names the oldest entry for a new tab's `?inbox=`.
    Inbox { url: String, key: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewSource {
    Rex {
        block: String,
    },
    Ultimate {
        url: String,
        conversation: u64,
    },
    /// Drawn from the model itself, so it needs no fetch.
    Inbox {
        entries: Vec<ultimate::InboxEntry>,
    },
}

pub struct Inputs<'a> {
    pub rex: &'a [rex::Session],
    pub ultimates: &'a [ultimate::Instance],
    pub registry: &'a Registry,
    pub worktrees: &'a Worktrees,
    pub prs: &'a PullRequests,
    pub seen: &'a Seen,
    pub now: u64,
}

/// An Ultimate instance lists busy, asking or failing conversations and those with recent input.
const RECENT_INPUT_MS: u64 = 2 * 3_600_000;
const MAX_CONVERSATIONS: usize = 5;

fn seen(values: &Seen, key: &str) -> u64 {
    values.get(key).copied().unwrap_or_default()
}

fn attention(record: &Record, viewed: u64) -> bool {
    record.state == "blocked" || (record.state == "done" && record.activity() > viewed)
}

fn state<'a>(states: impl Iterator<Item = &'a str>) -> String {
    states
        .max_by_key(|state| match *state {
            "blocked" => 5,
            "working" => 4,
            "done" => 3,
            "idle" => 2,
            _ => 1,
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn row_order(a: &Row, b: &Row) -> Ordering {
    b.attention
        .cmp(&a.attention)
        .then_with(|| b.last_viewed.cmp(&a.last_viewed))
        .then_with(|| b.last_activity.cmp(&a.last_activity))
        .then_with(|| a.name.cmp(&b.name))
}

fn child_order(a: &Row, b: &Row) -> Ordering {
    b.attention
        .cmp(&a.attention)
        .then_with(|| b.last_activity.cmp(&a.last_activity))
        .then_with(|| a.name.cmp(&b.name))
}

/// Worktree identity is based on the most common git toplevel, not cwd string equality.
fn session_tree<'a>(blocks: &[&Block], trees: &'a Worktrees) -> Option<&'a Worktree> {
    let mut counts: BTreeMap<&str, (usize, &Worktree)> = BTreeMap::new();
    for tree in blocks.iter().filter_map(|block| trees.get(block.cwd()?)) {
        let entry = counts.entry(&tree.path).or_insert((0, tree));
        entry.0 += 1;
    }
    counts
        .into_values()
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.path.cmp(&a.1.path)))
        .map(|(_, tree)| tree)
}

fn rex_jump(session: &rex::Session, block: Option<&Block>) -> JumpTarget {
    let location = block.map(|block| session.location(block));
    JumpTarget::Rex {
        session: session.id.clone(),
        window: location
            .as_ref()
            .and_then(|location| location.window.clone()),
        block: location.map(|location| location.block),
    }
}

fn record_details(blocks: &[&Block]) -> Vec<String> {
    blocks
        .iter()
        .flat_map(|block| block.records())
        .filter_map(|record| record.msg.clone())
        .collect()
}

fn conversation_name(conversation: &Conversation) -> String {
    if conversation.id == 1 {
        "@ultimate".into()
    } else {
        conversation
            .label
            .as_ref()
            .or(conversation.title.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("#{}", conversation.id))
    }
}

fn conversation_row(
    instance: &ultimate::Instance,
    conversation: &Conversation,
    inputs: &Inputs<'_>,
    location: Option<&Location>,
) -> Row {
    let key = format!("ult:{}#{}", instance.project, conversation.id);
    let status = instance.status.as_ref();
    let question = status
        .and_then(|status| status.questions.first.as_ref())
        .filter(|q| q.conversation == conversation.id);
    let failure = status.is_some_and(|status| {
        status.failures.iter().any(|failure| {
            failure
                .get("conversation")
                .and_then(serde_json::Value::as_u64)
                == Some(conversation.id)
        })
    });
    let attention = question.is_some() || failure;
    Row {
        last_viewed: seen(inputs.seen, &key).max(conversation.input_at.unwrap_or_default()),
        key,
        kind: Kind::Conversation,
        name: conversation_name(conversation),
        state: if attention {
            "blocked"
        } else if conversation.busy {
            "working"
        } else {
            "idle"
        }
        .into(),
        attention,
        last_activity: conversation.input_at.unwrap_or_default(),
        metadata: Metadata {
            project: Some(instance.project.clone()),
            worktree: Some(conversation.cwd.clone()),
            ..Metadata::default()
        },
        jump: JumpTarget::Ultimate {
            url: instance.url(),
            conversation: conversation.id,
            rex: location.cloned(),
        },
        preview: Some(PreviewSource::Ultimate {
            url: instance.url(),
            conversation: conversation.id,
        }),
        details: conversation
            .title
            .iter()
            .chain(conversation.label.iter())
            .cloned()
            .chain(question.map(|q| q.text.clone()))
            .collect(),
        children: Vec::new(),
    }
}

/// `Inbox  N` for an instance with open questions or items. Only questions are attention, as
/// for the instance; items wait quietly.
fn inbox_row(instance: &ultimate::Instance, parent: &str, inputs: &Inputs<'_>) -> Option<Row> {
    let oldest = instance.inbox.first()?;
    let key = format!("{parent}/inbox");
    let questions = instance
        .inbox
        .iter()
        .filter(|entry| entry.kind == "question")
        .count();
    Some(Row {
        last_viewed: seen(inputs.seen, &key),
        key,
        kind: Kind::Inbox,
        name: "Inbox".into(),
        state: if questions > 0 { "blocked" } else { "idle" }.into(),
        attention: questions > 0,
        last_activity: instance
            .inbox
            .iter()
            .map(|entry| entry.at)
            .max()
            .unwrap_or_default(),
        metadata: Metadata {
            project: Some(instance.project.clone()),
            agent_count: instance.inbox.len(),
            ..Metadata::default()
        },
        jump: JumpTarget::Inbox {
            url: instance.url(),
            key: oldest.key.clone(),
        },
        preview: Some(PreviewSource::Inbox {
            entries: instance.inbox.clone(),
        }),
        details: std::iter::once("inbox".to_owned())
            .chain(instance.inbox.iter().map(|entry| entry.title.clone()))
            .collect(),
        children: Vec::new(),
    })
}

pub fn build(inputs: &Inputs<'_>) -> Model {
    let mut folded: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut locations = BTreeMap::new();
    let mut host_activity: BTreeMap<&str, u64> = BTreeMap::new();
    let mut host_details: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for session in inputs.rex {
        for block in &session.blocks {
            if block
                .records()
                .iter()
                .any(|record| record.app == "ultimate")
                && let Some(instance) = inputs.ultimates.iter().find(|instance| {
                    Some(instance.pid) == block.foreground().and_then(|process| process.pid)
                })
            {
                folded.insert((session.id.as_str(), block.id.as_str()));
                host_details
                    .entry(&instance.project)
                    .or_default()
                    .extend(record_details(&[block]));
                locations
                    .entry(instance.project.as_str())
                    .or_insert_with(|| session.location(block));
                let activity = block
                    .records()
                    .iter()
                    .map(Record::activity)
                    .max()
                    .unwrap_or_default();
                host_activity
                    .entry(&instance.project)
                    .and_modify(|time| *time = (*time).max(activity))
                    .or_insert(activity);
            }
        }
    }
    let mut ultimates = Vec::new();
    for instance in inputs.ultimates {
        let location = locations.get(instance.project.as_str());
        let key = format!("ult:{}", instance.project);
        let mut all: Vec<Row> = instance
            .conversations
            .iter()
            .map(|conversation| conversation_row(instance, conversation, inputs, location))
            .collect();
        let last_viewed = all
            .iter()
            .map(|row| row.last_viewed)
            .max()
            .unwrap_or_default()
            .max(seen(inputs.seen, &key));
        let last_activity = all
            .iter()
            .map(|row| row.last_activity)
            .max()
            .unwrap_or_default()
            .max(
                host_activity
                    .get(instance.project.as_str())
                    .copied()
                    .unwrap_or_default(),
            );
        let mut details: Vec<String> = all
            .iter()
            .flat_map(|row| std::iter::once(row.name.clone()).chain(row.details.clone()))
            .collect();
        if let Some(question) = instance
            .status
            .as_ref()
            .and_then(|status| status.questions.first.as_ref())
        {
            details.push(question.text.clone());
        }
        details.extend(
            host_details
                .get(instance.project.as_str())
                .into_iter()
                .flatten()
                .cloned(),
        );
        all = all
            .into_iter()
            .zip(&instance.conversations)
            .filter_map(|(row, conversation)| {
                let relevant = conversation.busy
                    || row.attention
                    || (!conversation.concluded.unwrap_or(false)
                        && conversation.input_at.is_some_and(|time| {
                            time >= inputs.now.saturating_sub(RECENT_INPUT_MS)
                        }));
                relevant.then_some(row)
            })
            .collect();
        all.sort_by(child_order);
        let agent_count = all.len();
        let hidden = agent_count.saturating_sub(MAX_CONVERSATIONS);
        all.truncate(MAX_CONVERSATIONS);
        // The Inbox leads the instance's sub-rows; it is not one of the conversations the limit cuts.
        let inbox = inbox_row(instance, &key, inputs);
        let conversation = all
            .first()
            .and_then(|row| match row.jump {
                JumpTarget::Ultimate { conversation, .. } => Some(conversation),
                _ => None,
            })
            .unwrap_or(1);
        let attention = instance
            .status
            .as_ref()
            .is_some_and(|status| status.questions.count > 0 || !status.failures.is_empty());
        let busy = instance
            .status
            .as_ref()
            .is_some_and(|status| status.busy > 0)
            || instance
                .conversations
                .iter()
                .any(|conversation| conversation.busy);
        ultimates.push(Row {
            key,
            kind: Kind::Ultimate,
            name: instance.label.clone().unwrap_or_else(|| {
                Path::new(&instance.project).file_name().map_or_else(
                    || instance.project.clone(),
                    |name| name.to_string_lossy().into_owned(),
                )
            }),
            state: if attention {
                "blocked"
            } else if busy {
                "working"
            } else if !instance.errors.is_empty() {
                "unknown"
            } else {
                "idle"
            }
            .into(),
            attention,
            last_viewed,
            last_activity,
            metadata: Metadata {
                project: Some(instance.project.clone()),
                port: Some(instance.port),
                origin: Some(instance.url()),
                hub: instance.hub.clone(),
                agent_count,
                hidden,
                errors: instance.errors.clone(),
                ..Metadata::default()
            },
            jump: JumpTarget::Ultimate {
                url: instance.url(),
                conversation,
                rex: location.cloned(),
            },
            preview: Some(PreviewSource::Ultimate {
                url: instance.url(),
                conversation,
            }),
            details,
            children: {
                let mut children = if agent_count > 1 { all } else { Vec::new() };
                if let Some(inbox) = inbox {
                    children.insert(0, inbox);
                }
                children
            },
        });
    }
    let mut sessions = Vec::new();
    for session in inputs.rex {
        let blocks: Vec<&Block> = session
            .blocks
            .iter()
            .filter(|block| !folded.contains(&(session.id.as_str(), block.id.as_str())))
            .collect();
        if !session.blocks.is_empty() && blocks.is_empty() {
            continue;
        }
        let key = format!("rex:{}", session.id);
        let viewed = seen(inputs.seen, &key);
        let records: Vec<&Record> = blocks.iter().flat_map(|block| block.records()).collect();
        let all_blocks: Vec<_> = session.blocks.iter().collect();
        let tree = session_tree(&all_blocks, inputs.worktrees);
        let pr = tree
            .and_then(|tree| {
                inputs
                    .prs
                    .get(&tree.repo)?
                    .iter()
                    .find(|pr| pr.head_ref_name == tree.branch)
            })
            .map(|pr| Pr {
                number: pr.number,
                url: pr.url.clone(),
                colour: pr.colour(),
            });
        let mut children: Vec<Row> = blocks
            .iter()
            .filter_map(|block| {
                let agent = block
                    .records()
                    .iter()
                    .filter(|record| record.is_agent())
                    .max_by_key(|record| record.activity())?;
                Some(Row {
                    key: format!("{key}/{}", block.id),
                    kind: Kind::Agent,
                    name: agent
                        .msg
                        .clone()
                        .filter(|msg| !msg.is_empty())
                        .unwrap_or_else(|| agent.app.clone()),
                    state: state(block.records().iter().map(|record| record.state.as_str())),
                    attention: block
                        .records()
                        .iter()
                        .any(|record| attention(record, viewed)),
                    last_viewed: viewed,
                    last_activity: block
                        .records()
                        .iter()
                        .map(Record::activity)
                        .max()
                        .unwrap_or_default(),
                    metadata: Metadata {
                        worktree: block.cwd().map(str::to_owned),
                        ..Metadata::default()
                    },
                    jump: rex_jump(session, Some(block)),
                    preview: Some(PreviewSource::Rex {
                        block: block.id.clone(),
                    }),
                    details: record_details(&[block]),
                    children: Vec::new(),
                })
            })
            .collect();
        children.sort_by(child_order);
        let agent_count = children.len();
        let selected = children
            .first()
            .and_then(|child| match &child.preview {
                Some(PreviewSource::Rex { block }) => blocks
                    .iter()
                    .find(|candidate| candidate.id == *block)
                    .copied(),
                _ => None,
            })
            .or_else(|| blocks.first().copied());
        let cwds: Vec<_> = all_blocks.iter().filter_map(|block| block.cwd()).collect();
        sessions.push(Row {
            key,
            kind: Kind::Rex,
            name: session.label.clone(),
            state: state(records.iter().map(|record| record.state.as_str())),
            attention: records.iter().any(|record| attention(record, viewed)),
            last_viewed: viewed,
            last_activity: records
                .iter()
                .map(|record| record.activity())
                .max()
                .unwrap_or_default(),
            metadata: Metadata {
                branch: tree.map(|tree| tree.branch.clone()),
                worktree: tree.map(|tree| tree.path.clone()),
                pr,
                agent_count,
                apps: registry::apps_for(inputs.registry, &cwds)
                    .into_iter()
                    .map(|app| Endpoint {
                        port: app.port,
                        origin: app.origin.clone(),
                    })
                    .collect(),
                errors: session.error.iter().cloned().collect(),
                ..Metadata::default()
            },
            jump: rex_jump(session, selected),
            preview: selected.map(|block| PreviewSource::Rex {
                block: block.id.clone(),
            }),
            details: record_details(&blocks),
            children: if agent_count > 1 {
                children
            } else {
                Vec::new()
            },
        });
    }
    ultimates.sort_by(row_order);
    sessions.sort_by(row_order);
    Model {
        groups: vec![
            Group {
                kind: Kind::Ultimate,
                rows: ultimates,
            },
            Group {
                kind: Kind::Rex,
                rows: sessions,
            },
        ],
    }
}

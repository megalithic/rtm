use crate::{
    needs,
    ultimate::{self, InboxEntry},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, thread};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Question {
    pub text: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<OptionLabel>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OptionLabel {
    pub label: String,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub base: InboxEntry,
    pub text: String,
    pub url: Option<String>,
    pub path: Option<String>,
    pub questions: Vec<Question>,
    pub answered: Value,
}
impl Entry {
    pub fn answerable(&self) -> Option<(&Question, Vec<String>)> {
        if self.base.kind != "question" {
            return None;
        }
        let open: Vec<_> = self
            .questions
            .iter()
            .enumerate()
            .filter(|(i, _)| self.answered.get(i.to_string()).is_none())
            .map(|(_, q)| q)
            .collect();
        let [q] = open.as_slice() else { return None };
        let options = match q.kind.as_str() {
            "confirm" => vec!["Yes".into(), "No".into()],
            "choice" if !q.options.is_empty() => {
                q.options.iter().map(|o| o.label.clone()).collect()
            }
            _ => return None,
        };
        Some((q, options))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Waiting,
    Working,
    Quiet,
}
impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting on you",
            Self::Working => "Working",
            Self::Quiet => "Quiet",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Row {
    pub section: Section,
    pub host: String,
    pub project: String,
    pub url: String,
    pub title: String,
    pub at: u64,
    pub entry: Option<Entry>,
    pub conversation: Option<u64>,
    pub error: bool,
}
impl Row {
    pub fn key(&self) -> String {
        self.entry
            .as_ref()
            .map(|e| e.base.key.clone())
            .unwrap_or_else(|| format!("conversation:{:?}", self.conversation))
    }
    pub fn kind(&self) -> &str {
        if self.error {
            "error"
        } else if self.entry.is_none() && self.conversation.is_none() {
            "empty"
        } else {
            self.entry
                .as_ref()
                .map_or("working", |e| e.base.kind.as_str())
        }
    }
}
pub fn entries(messages: Option<&Value>, items: Option<&Value>) -> Vec<Entry> {
    ultimate::inbox(messages, items)
        .into_iter()
        .map(|base| {
            let (doc, list) = if base.kind == "question" {
                (messages, "messages")
            } else {
                (items, "items")
            };
            let id = base
                .key
                .split_once(':')
                .and_then(|(_, n)| n.parse::<u64>().ok());
            let raw = doc
                .and_then(|d| d[list].as_array())
                .and_then(|xs| xs.iter().find(|v| v["id"].as_u64() == id))
                .cloned()
                .unwrap_or(Value::Null);
            Entry {
                base,
                text: raw["text"].as_str().unwrap_or_default().into(),
                url: raw["url"].as_str().map(str::to_owned),
                path: raw["path"].as_str().map(str::to_owned),
                questions: serde_json::from_value(raw["questions"].clone()).unwrap_or_default(),
                answered: raw["answered"].clone(),
            }
        })
        .collect()
}
pub fn project(
    host: &str,
    project: &str,
    url: &str,
    entries: Vec<Entry>,
    conversations: Vec<ultimate::Conversation>,
    error: Option<String>,
    now: u64,
) -> Vec<Row> {
    let base = Row {
        section: Section::Waiting,
        host: host.into(),
        project: project.into(),
        url: url.into(),
        title: String::new(),
        at: now,
        entry: None,
        conversation: None,
        error: false,
    };
    let mut rows: Vec<_> = entries
        .into_iter()
        .map(|entry| Row {
            section: if now.saturating_sub(entry.base.at) >= 86_400_000 {
                Section::Quiet
            } else {
                Section::Waiting
            },
            title: entry.base.title.clone(),
            at: entry.base.at,
            entry: Some(entry),
            ..base.clone()
        })
        .collect();
    rows.extend(conversations.into_iter().filter(|c| c.busy).map(|c| {
        Row {
            section: Section::Working,
            title: c
                .title
                .or(c.label)
                .unwrap_or_else(|| format!("Conversation {}", c.id)),
            at: c.input_at.unwrap_or(now),
            conversation: Some(c.id),
            ..base.clone()
        }
    }));
    if let Some(error) = error {
        rows.push(Row {
            title: format!("unreachable: {error}"),
            error: true,
            ..base.clone()
        });
    }
    if rows.is_empty() {
        rows.push(Row {
            section: Section::Quiet,
            title: "Nothing waiting".into(),
            ..base
        });
    }
    rows
}
pub fn sort(rows: &mut [Row], host: &str) {
    rows.sort_by(|a, b| {
        (
            a.section,
            a.host != host,
            &a.host,
            &a.project,
            a.at,
            &a.title,
        )
            .cmp(&(
                b.section,
                b.host != host,
                &b.host,
                &b.project,
                b.at,
                &b.title,
            ))
    });
}
pub fn load(home: &Path) -> Result<Vec<Row>> {
    let host = needs::host_name();
    let targets = needs::targets(home, &host)?;
    let mut rows = thread::scope(|s| {
        let workers: Vec<_> = targets
            .into_iter()
            .map(|((host, name, url), timeout)| {
                s.spawn(move || {
                    let result = (|| -> Result<_> {
                        let messages = ultimate::read_doc_within(&url, "board", timeout)?;
                        let items = ultimate::read_doc_within(&url, "inbox-items.items", timeout)?;
                        let conversations = ultimate::action_within(
                            &url,
                            "codemode/actions/read",
                            serde_json::json!({"op":"conversations","args":[]}),
                            timeout,
                        )?
                        .ok_or_else(|| anyhow::anyhow!("no conversations API"))?;
                        Ok((
                            entries(messages.as_ref(), items.as_ref()),
                            serde_json::from_value(conversations)?,
                        ))
                    })();
                    match result {
                        Ok((entries, conversations)) => project(
                            &host,
                            &name,
                            &url,
                            entries,
                            conversations,
                            None,
                            crate::now_ms(),
                        ),
                        Err(e) => project(
                            &host,
                            &name,
                            &url,
                            vec![],
                            vec![],
                            Some(format!("{e:#}")),
                            crate::now_ms(),
                        ),
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("inbox loader panicked"))
            .collect::<Vec<_>>()
    });
    sort(&mut rows, &host);
    Ok(rows)
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Instance {
    pub pid: u32,
    pub port: u16,
    pub project: String,
    pub label: Option<String>,
    #[serde(default)]
    pub hub: Option<String>,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    #[serde(default)]
    pub errors: Vec<String>,
    /// Open questions to the user and open Inbox items, oldest first.
    #[serde(default)]
    pub inbox: Vec<InboxEntry>,
}

/// One thing in an instance's Inbox: a question to the user or an item an extension provides.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InboxEntry {
    /// `message:<id>` or `inbox-items:<id>`, the key `?inbox=` selects.
    pub key: String,
    /// `question`, `action`, `artifact` or `link`.
    pub kind: String,
    pub title: String,
    /// A question's first question line, when it asks structured questions.
    pub detail: Option<String>,
    pub at: u64,
}

#[derive(Deserialize)]
struct Message {
    id: u64,
    #[serde(default)]
    to: Value,
    #[serde(default)]
    text: String,
    #[serde(default)]
    at: u64,
    #[serde(default)]
    reply: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    questions: Vec<MessageQuestion>,
}

#[derive(Deserialize)]
struct MessageQuestion {
    text: String,
}

#[derive(Deserialize)]
struct Item {
    id: u64,
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    at: u64,
    #[serde(default)]
    done: Option<Value>,
}

/// A message's title as the Inbox shows it: a bold lead, or else its first line without Markdown marks.
pub fn lead(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("**")
        && let Some(end) = rest.find("**")
        && end > 0
    {
        return rest[..end].trim().to_owned();
    }
    trimmed
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches(['#', '>', '-', '*', ' ', '\t'])
        .replace(['*', '_', '`'], "")
        .trim()
        .to_owned()
}

/// The Inbox from the Messages document and the inbox-items document (either may be absent),
/// oldest first. A question is open as the Inbox counts it: to the user, expecting a reply,
/// delivered; an item is open until it is done.
pub fn inbox(messages: Option<&Value>, items: Option<&Value>) -> Vec<InboxEntry> {
    let messages: Vec<Message> = messages
        .and_then(|doc| serde_json::from_value(doc.get("messages")?.clone()).ok())
        .unwrap_or_default();
    let items: Vec<Item> = items
        .and_then(|doc| serde_json::from_value(doc.get("items")?.clone()).ok())
        .unwrap_or_default();
    let mut entries: Vec<InboxEntry> = messages
        .into_iter()
        .filter(|m| m.to == "user" && m.reply && m.state == "delivered")
        .map(|m| InboxEntry {
            key: format!("message:{}", m.id),
            kind: "question".into(),
            title: lead(&m.text),
            detail: m.questions.first().map(|q| q.text.clone()),
            at: m.at,
        })
        .chain(
            items
                .into_iter()
                .filter(|item| {
                    item.done
                        .as_ref()
                        .is_none_or(|done| done.is_null() || done == &Value::Bool(false))
                })
                .map(|item| InboxEntry {
                    key: format!("inbox-items:{}", item.id),
                    kind: item.kind,
                    title: item.title,
                    detail: None,
                    at: item.at,
                }),
        )
        .collect();
    entries.sort_by_key(|entry| entry.at);
    entries
}

/// An agent for one hub: no redirects, and macOS's own TLS for remote hubs on tailnet `https://` URLs.
fn agent(timeout: Duration, status_as_error: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(status_as_error)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

/// A conversation document, or `None` when the instance does not have it (an extension it lacks).
fn read_doc(url: &str, kind: &str) -> Result<Option<Value>> {
    read_doc_within(url, kind, Duration::from_secs(1))
}

pub fn read_doc_within(url: &str, kind: &str, timeout: Duration) -> Result<Option<Value>> {
    let body: Value = agent(timeout, false)
        .post(format!("{url}/api/extensions/codemode/actions/read"))
        .header("Origin", url)
        .header("Content-Type", "application/json")
        .send_json(json!({"args": {"op": "doc", "args": [kind, 1]}}))?
        .body_mut()
        .read_json()?;
    doc_result(body)
}

/// An extension action's `{result}`, or `None` when the instance lacks that extension action.
pub fn action_within(
    url: &str,
    action: &str,
    args: Value,
    timeout: Duration,
) -> Result<Option<Value>> {
    let body: Value = agent(timeout, false)
        .post(format!("{url}/api/extensions/{action}"))
        .header("Origin", url)
        .header("Content-Type", "application/json")
        .send_json(json!({"args": args}))?
        .body_mut()
        .read_json()?;
    if let Some(result) = body.get("result") {
        return Ok(Some(result.clone()));
    }
    match body.get("error").and_then(Value::as_str) {
        Some("Unknown extension action") => Ok(None),
        Some(error) => anyhow::bail!("{error}"),
        None => anyhow::bail!("unexpected response"),
    }
}

/// `{result}` is the document; an unknown document is `None`; any other error is an error.
pub fn doc_result(body: Value) -> Result<Option<Value>> {
    if let Some(result) = body.get("result") {
        return Ok(Some(result.clone()));
    }
    match body.get("error").and_then(Value::as_str) {
        Some(error) if error.starts_with("Unknown conversation document") => Ok(None),
        Some(error) => anyhow::bail!("{error}"),
        None => anyhow::bail!("unexpected response"),
    }
}

impl Instance {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Status {
    #[serde(default)]
    pub busy: u64,
    #[serde(default)]
    pub questions: Questions,
    #[serde(default)]
    pub failures: Vec<Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Questions {
    #[serde(default)]
    pub count: u64,
    pub first: Option<Question>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Question {
    pub text: String,
    pub conversation: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Conversation {
    pub id: u64,
    pub title: Option<String>,
    pub label: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub busy: bool,
    #[serde(rename = "inputAt")]
    pub input_at: Option<u64>,
    pub concluded: Option<bool>,
    pub parent: Option<Parent>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Parent {
    #[serde(rename = "conversationId")]
    pub conversation_id: u64,
    pub at: u64,
}

#[derive(Deserialize)]
struct Response<T> {
    result: T,
}

fn read_action<T: DeserializeOwned>(url: &str, action: &str, args: Value) -> Result<T> {
    let response: Response<T> = agent(Duration::from_secs(1), true)
        .post(format!("{url}/api/extensions/{action}"))
        .header("Origin", url)
        .header("Content-Type", "application/json")
        .send_json(json!({"args": args}))?
        .body_mut()
        .read_json()?;
    Ok(response.result)
}

/// Newest first, deliberately preserving entry payloads for the future preview renderer.
pub fn entries(url: &str, conversation: u64) -> Result<Vec<Value>> {
    read_action(
        url,
        "codemode/actions/read",
        json!({"op":"entries", "args":[conversation, {"limit":40}]}),
    )
}

pub fn user_home() -> Result<PathBuf> {
    Ok(std::env::var_os("ULTIMATE_USER_HOME")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::home_dir()
                .context("home directory")?
                .join(".ultimate"),
        ))
}

fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // Signal 0 only checks existence/permission; it never signals the process.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn discover(home: &Path) -> Result<Vec<Instance>> {
    let projects = match fs::read_dir(home.join("projects")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut instances: Vec<Instance> = projects
        .filter_map(|entry| {
            let bytes = fs::read(entry.ok()?.path().join("ultimate.lock")).ok()?;
            let instance: Instance = serde_json::from_slice(&bytes).ok()?;
            (instance.port > 0 && alive(instance.pid)).then_some(instance)
        })
        .collect();
    let primary = fs::read(home.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("fleet")?
                .get("primary")?
                .as_str()
                .map(expand_home)
        });
    let paths: Vec<PathBuf> = instances.iter().map(|i| expand_home(&i.project)).collect();
    for (index, instance) in instances.iter_mut().enumerate() {
        let project = &paths[index];
        instance.hub = if primary.as_ref() == Some(project) {
            Some("primary".into())
        } else if paths
            .iter()
            .any(|other| other != project && other.starts_with(project))
        {
            Some("secondary".into())
        } else {
            None
        };
    }
    instances.sort_by(|a, b| a.project.cmp(&b.project));
    Ok(instances)
}

fn expand_home(path: &str) -> PathBuf {
    match (dirs::home_dir(), path.strip_prefix("~/")) {
        (Some(home), Some(suffix)) => home.join(suffix),
        (Some(home), _) if path == "~" => home,
        _ => PathBuf::from(path),
    }
}

pub fn load() -> Result<Vec<Instance>> {
    let instances = discover(&user_home()?)?;
    thread::scope(|scope| {
        let workers: Vec<_> = instances
            .into_iter()
            .map(|mut instance| {
                scope.spawn(move || {
                    let url = instance.url();
                    let (status, conversations, messages, items) = thread::scope(|scope| {
                        let status = scope.spawn(|| {
                            read_action::<Status>(&url, "status/actions/status", json!({}))
                        });
                        let messages = scope.spawn(|| read_doc(&url, "board"));
                        let items = scope.spawn(|| read_doc(&url, "inbox-items.items"));
                        let conversations = read_action::<Vec<Conversation>>(
                            &url,
                            "codemode/actions/read",
                            json!({"op":"conversations", "args":[]}),
                        );
                        (
                            status.join().expect("status worker panicked"),
                            conversations,
                            messages.join().expect("messages worker panicked"),
                            items.join().expect("inbox items worker panicked"),
                        )
                    });
                    let messages = messages.unwrap_or_else(|error| {
                        instance.errors.push(format!("messages: {error:#}"));
                        None
                    });
                    let items = items.unwrap_or_else(|error| {
                        instance.errors.push(format!("inbox items: {error:#}"));
                        None
                    });
                    instance.inbox = inbox(messages.as_ref(), items.as_ref());
                    match status {
                        Ok(status) => instance.status = Some(status),
                        Err(error) => instance.errors.push(format!("status: {error:#}")),
                    }
                    match conversations {
                        Ok(conversations) => instance.conversations = conversations,
                        Err(error) => instance.errors.push(format!("conversations: {error:#}")),
                    }
                    instance
                })
            })
            .collect();
        Ok(workers
            .into_iter()
            .map(|worker| worker.join().expect("instance worker panicked"))
            .collect())
    })
}

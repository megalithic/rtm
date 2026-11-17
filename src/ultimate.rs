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
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_redirects(0)
        .build()
        .new_agent();
    let response: Response<T> = agent
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
                    let (status, conversations) = thread::scope(|scope| {
                        let status = scope.spawn(|| {
                            read_action::<Status>(&url, "status/actions/status", json!({}))
                        });
                        let conversations = read_action::<Vec<Conversation>>(
                            &url,
                            "codemode/actions/read",
                            json!({"op":"conversations", "args":[]}),
                        );
                        (
                            status.join().expect("status worker panicked"),
                            conversations,
                        )
                    });
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

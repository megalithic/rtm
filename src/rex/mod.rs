use crate::Timestamp;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, deserialize_with = "list")]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub view: Option<View>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct View {
    pub active_window_id: Option<String>,
    #[serde(default, deserialize_with = "list")]
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Window {
    pub window_id: String,
    #[serde(default, deserialize_with = "list")]
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Layer {
    #[serde(default, deserialize_with = "list")]
    pub blocks: Vec<ViewBlock>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ViewBlock {
    pub block_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Block {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub process: Option<Process>,
    pub status: Option<Status>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Process {
    pub foreground: Option<Foreground>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Foreground {
    pub pid: Option<u32>,
    pub name: Option<String>,
    pub argv0: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Status {
    #[serde(default, deserialize_with = "list")]
    pub records: Vec<Record>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Record {
    pub app: String,
    #[serde(default)]
    pub state: String,
    pub msg: Option<String>,
    pub updated_at: Option<Timestamp>,
}

impl Record {
    pub fn activity(&self) -> u64 {
        self.updated_at.as_ref().map_or(0, Timestamp::millis)
    }

    pub fn is_agent(&self) -> bool {
        matches!(self.app.as_str(), "pi" | "agent")
    }
}

impl Block {
    pub fn records(&self) -> &[Record] {
        self.status.as_ref().map_or(&[], |status| &status.records)
    }

    pub fn foreground(&self) -> Option<&Foreground> {
        self.process.as_ref()?.foreground.as_ref()
    }

    pub fn cwd(&self) -> Option<&str> {
        self.foreground()?.cwd.as_deref()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Location {
    pub session: String,
    pub window: Option<String>,
    pub block: String,
}

impl Session {
    pub fn location(&self, block: &Block) -> Location {
        let window = self.view.as_ref().and_then(|view| {
            view.windows
                .iter()
                .find(|window| {
                    window
                        .layers
                        .iter()
                        .any(|layer| layer.blocks.iter().any(|item| item.block_id == block.id))
                })
                .map(|window| window.window_id.clone())
        });
        Location {
            session: self.id.clone(),
            window,
            block: block.id.clone(),
        }
    }
}

// Rex's Lua encoder represents empty tables as {}, including empty lists.
fn list<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<T>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum List<T> {
        Items(Vec<T>),
        Empty(BTreeMap<String, serde::de::IgnoredAny>),
    }
    match List::deserialize(deserializer)? {
        List::Items(items) => Ok(items),
        List::Empty(map) if map.is_empty() => Ok(Vec::new()),
        List::Empty(_) => Err(serde::de::Error::custom(
            "expected a list or empty Lua table",
        )),
    }
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Session>> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let sessions = list(&mut deserializer)?;
    deserializer.end()?;
    Ok(sessions)
}

pub fn load() -> Result<Vec<Session>> {
    let binary = which_rex().context("rex is not on PATH and REX_BIN_DIR/rex is unavailable")?;
    let mut child = Command::new(binary)
        .args(["--autostart=false", "do", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start read-only Rex snapshot")?;
    child
        .stdin
        .take()
        .context("Rex stdin")?
        .write_all(include_str!("snapshot.lua").as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("Rex snapshot: {}", String::from_utf8_lossy(&output.stderr));
    }
    decode(&output.stdout).context("decode Rex snapshot")
}

pub fn which_rex() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("rex"))
        .chain(std::env::var_os("REX_BIN_DIR").map(|dir| PathBuf::from(dir).join("rex")))
        .find(|path| path.is_file())
}

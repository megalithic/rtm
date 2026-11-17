//! Read-only data sources and pure launcher model. No terminal UI or session actions.
pub mod cache;
pub mod git;
pub mod model;
pub mod registry;
pub mod rex;
pub mod search;
pub mod seen;
pub mod snapshot;
pub mod ultimate;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Rex uses RFC3339 with offsets and fractional seconds; Ultimate uses epoch ms.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum Timestamp {
    Millis(u64),
    Text(String),
}

impl Timestamp {
    pub fn millis(&self) -> u64 {
        match self {
            Self::Millis(ms) => *ms,
            Self::Text(text) => chrono::DateTime::parse_from_rfc3339(text)
                .ok()
                .and_then(|date| u64::try_from(date.timestamp_millis()).ok())
                .unwrap_or_default(),
        }
    }
}

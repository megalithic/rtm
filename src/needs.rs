//! What waits on the user across every Ultimate hub: this Mac's running instances and the other
//! machines' hubs from `remotes` in `~/.ultimate/settings.json`. Each count is rtm's own Inbox
//! (`ultimate::inbox`), so the menu bar, Rex and the picker never disagree.
use crate::ultimate::{self, InboxEntry};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path, thread, time::Duration};

#[derive(Debug, Serialize)]
pub struct Needs {
    /// This Mac's short host name.
    pub host: String,
    /// Everything waiting on the user across the hubs that answered.
    pub total: usize,
    /// This Mac's instances first, then each remote host's, by project.
    pub hubs: Vec<Hub>,
}

#[derive(Debug, Serialize)]
pub struct Hub {
    pub host: String,
    pub project: String,
    pub url: String,
    /// `None` when the hub could not be read: unreachable is never 0.
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Open questions and Inbox items, oldest first; `<url>/?inbox=<key>` opens one.
    pub inbox: Vec<InboxEntry>,
}

/// A remote hub: `(host, project, url)`.
pub type Remote = (String, String, String);

/// Every `remotes.<host>.<project>` URL in the settings, except this host's own.
pub fn remotes(settings: &Value, host: &str) -> Vec<Remote> {
    let mut remotes: Vec<Remote> = settings
        .get("remotes")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| !name.eq_ignore_ascii_case(host))
        .flat_map(|(name, projects)| {
            projects
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(move |(project, url)| {
                    Some((name.clone(), project.clone(), url.as_str()?.to_owned()))
                })
        })
        .collect();
    remotes.sort();
    remotes
}

/// The hub's Inbox, or an error that says which read failed.
pub fn read(url: &str, timeout: Duration) -> Result<Vec<InboxEntry>> {
    let (messages, items) = thread::scope(|scope| {
        let items = scope.spawn(|| ultimate::read_doc_within(url, "inbox-items.items", timeout));
        let messages = ultimate::read_doc_within(url, "board", timeout);
        (messages, items.join().expect("inbox items worker panicked"))
    });
    Ok(ultimate::inbox(messages?.as_ref(), items?.as_ref()))
}

fn hub(host: String, project: String, url: String, result: Result<Vec<InboxEntry>>) -> Hub {
    match result {
        Ok(inbox) => Hub {
            host,
            project,
            url,
            count: Some(inbox.len()),
            error: None,
            inbox,
        },
        Err(error) => Hub {
            host,
            project,
            url,
            count: None,
            error: Some(format!("{error:#}")),
            inbox: Vec::new(),
        },
    }
}

pub fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // gethostname fills a NUL-terminated name; `workbookpro.local` reads as `workbookpro`.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let name = if ok {
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    } else {
        String::new()
    };
    name.split('.').next().unwrap_or_default().to_owned()
}

pub fn load(home: &Path) -> Result<Needs> {
    let host = host_name();
    let settings = fs::read(home.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or(Value::Null);
    let locals: Vec<Remote> = ultimate::discover(home)?
        .into_iter()
        .map(|instance| {
            let project = instance.label.clone().unwrap_or_else(|| {
                Path::new(&instance.project).file_name().map_or_else(
                    || instance.project.clone(),
                    |name| name.to_string_lossy().into_owned(),
                )
            });
            (host.clone(), project, instance.url())
        })
        .collect();
    let targets: Vec<(Remote, Duration)> = locals
        .into_iter()
        .map(|local| (local, Duration::from_secs(1)))
        .chain(
            remotes(&settings, &host)
                .into_iter()
                .map(|remote| (remote, Duration::from_secs(3))),
        )
        .collect();
    let hubs: Vec<Hub> = thread::scope(|scope| {
        let workers: Vec<_> = targets
            .into_iter()
            .map(|((host, project, url), timeout)| {
                scope.spawn(move || {
                    let result = read(&url, timeout);
                    hub(host, project, url, result)
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("hub worker panicked"))
            .collect()
    });
    Ok(Needs {
        total: hubs.iter().filter_map(|hub| hub.count).sum(),
        host,
        hubs,
    })
}

use super::data::Row;
use crate::{
    automations::private_hub,
    needs,
    pick::actions::{Io, rex_bin},
    ultimate,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn guard(home: &Path, url: &str) -> Result<()> {
    if !private_hub(url) {
        bail!("not a private hub: {url}");
    }
    if !needs::targets(home, &needs::host_name())?
        .iter()
        .any(|((_, _, u), _)| u == url)
    {
        bail!("unknown hub: {url}");
    }
    Ok(())
}
pub fn request(row: &Row, operation: &str, option: Option<&str>) -> Result<(String, Value)> {
    let entry = row.entry.as_ref().context("not an inbox entry")?;
    let id: u64 = entry
        .base
        .key
        .split_once(':')
        .context("invalid key")?
        .1
        .parse()?;
    let (action, args) = match (operation, entry.base.kind.as_str()) {
        ("done", kind) if kind != "question" => ("inbox-items/actions/done", json!({"id":id})),
        ("dismiss", "question") => ("questions/actions/dismiss", json!({"messageId":id})),
        ("answer", "question") => {
            let (q, options) = entry
                .answerable()
                .context("answer this one in Ultimate (enter)")?;
            let option = option
                .filter(|o| options.iter().any(|label| label == o))
                .context("invalid option")?;
            (
                "questions/actions/answer",
                json!({"messageId":id,"answers":[{"question":q.text,"answer":option}]}),
            )
        }
        _ => bail!("not available for this row"),
    };
    Ok((action.into(), args))
}
pub fn write(home: &Path, row: &Row, operation: &str, option: Option<&str>) -> Result<()> {
    guard(home, &row.url)?;
    let (action, args) = request(row, operation, option)?;
    ultimate::action_within(&row.url, &action, args, Duration::from_secs(5))?
        .context("missing action")?;
    Ok(())
}
fn program(name: &str) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let candidate = if matches!(name, "terminal-browser" | "kitten") {
        PathBuf::from("/opt/homebrew/bin").join(name)
    } else {
        home.join(".local/share/mise/shims").join(name)
    };
    if candidate.is_file() {
        candidate.to_string_lossy().into_owned()
    } else {
        name.into()
    }
}
pub fn viewer(row: &Row) -> Result<Vec<String>> {
    if let Some(e) = &row.entry {
        if e.base.kind == "artifact"
            && let Some(path) = &e.path
        {
            let path = if path == "~" {
                dirs::home_dir().context("home directory")?
            } else if let Some(rest) = path.strip_prefix("~/") {
                dirs::home_dir().context("home directory")?.join(rest)
            } else {
                PathBuf::from(path)
            };
            // Only absolute paths: a path from a hub then never reads as a viewer's option.
            if !path.is_absolute() {
                bail!("not an absolute path");
            }
            if path.is_dir() {
                bail!("no viewer for a directory");
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let path = path.to_string_lossy().into_owned();
            return Ok(match ext.as_str() {
                "png" | "jpg" | "jpeg" | "gif" | "webp" => {
                    vec![program("kitten"), "icat".into(), "--hold".into(), path]
                }
                "md" | "diff" | "patch" => {
                    let command = if ext == "md" {
                        "\"$1\" \"$2\""
                    } else {
                        "\"$1\" < \"$2\""
                    };
                    vec![
                        "sh".into(),
                        "-c".into(),
                        format!("{command}; printf '\\n[press enter to close]'; read _"),
                        "sh".into(),
                        program(if ext == "md" { "glow" } else { "delta" }),
                        path,
                    ]
                }
                _ => vec![program("bat"), "--paging=always".into(), "--".into(), path],
            });
        }
        if e.base.kind == "link"
            && let Some(url) = &e.url
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            return Ok(vec![
                program("terminal-browser"),
                "open".into(),
                url.clone(),
            ]);
        }
    }
    let url = match &row.entry {
        Some(e) => format!("{}/?inbox={}", row.url, e.base.key),
        None => format!(
            "{}/?conversation={}",
            row.url,
            row.conversation.context("no target")?
        ),
    };
    Ok(vec![program("terminal-browser"), "open".into(), url])
}
pub fn split(
    io: &dyn Io,
    anchor: Option<&str>,
    previous: &mut Option<String>,
    row: &Row,
) -> Result<()> {
    let anchor = anchor.context("open in a split needs Rex")?;
    let viewer = viewer(row)?;
    let rex = rex_bin();
    if let Some(id) = previous.as_ref() {
        if let Err(e) = io.run(&rex, &["--autostart=false", "block", "close", id], None)
            && !e
                .to_string()
                .contains(&format!("block target \"{id}\" not found"))
        {
            return Err(e);
        }
        *previous = None;
    }
    let mut args = vec![
        "--autostart=false",
        "split",
        "-b",
        anchor,
        "--split",
        "right",
        "--ratio",
        "50",
        "--focus=false",
        "--json",
        "--",
    ];
    args.extend(viewer.iter().map(String::as_str));
    let output = io.run(&rex, &args, None)?;
    let value: Value = serde_json::from_str(&output)?;
    *previous = Some(
        value
            .get("block_ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str)
            .context("split returned no block_ids")?
            .into(),
    );
    Ok(())
}

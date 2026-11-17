//! Previews load on one worker thread. Each request carries a sequence number; the
//! worker drains its queue to the newest request, so fast movement never builds a backlog.
use crate::{model::PreviewSource, ultimate};
use ansi_to_tui::IntoText;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Output, Stdio},
    sync::mpsc::{Receiver, Sender, channel},
    thread,
    time::{Duration, Instant},
};

/// Lines kept from the bottom of a block's scrollback (a capture can be megabytes).
const TAIL_LINES: usize = 400;

pub struct Request {
    pub seq: u64,
    pub key: String,
    pub source: PreviewSource,
    pub session: Option<String>,
}

pub struct Response {
    pub seq: u64,
    pub key: String,
    pub text: Result<Text<'static>, String>,
    pub took: Duration,
}

pub fn spawn(wake: Sender<()>) -> (Sender<Request>, Receiver<Response>) {
    let (requests, inbox) = channel::<Request>();
    let (outbox, responses) = channel();
    thread::spawn(move || {
        while let Ok(mut request) = inbox.recv() {
            while let Ok(newer) = inbox.try_recv() {
                request = newer;
            }
            let start = Instant::now();
            let text = load(&request).map_err(|error| format!("{error:#}"));
            if outbox
                .send(Response {
                    seq: request.seq,
                    key: request.key,
                    text,
                    took: start.elapsed(),
                })
                .is_err()
            {
                return;
            }
            let _ = wake.send(());
        }
    });
    (requests, responses)
}

fn load(request: &Request) -> anyhow::Result<Text<'static>> {
    match &request.source {
        PreviewSource::Rex { block } => {
            let session = request.session.as_deref().unwrap_or_default();
            let mut command = Command::new(super::actions::rex_bin());
            command.args([
                "--autostart=false",
                "capture",
                "-s",
                session,
                "-b",
                block,
                "--format",
                "vt",
            ]);
            let output = output_within(command, CAPTURE_LIMIT)?;
            if !output.status.success() {
                anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
            }
            Ok(vt(&output.stdout))
        }
        PreviewSource::Ultimate { url, conversation } => {
            Ok(conversation_text(&ultimate::entries(url, *conversation)?))
        }
    }
}

/// A stuck `rex capture` must not hold the preview worker: later requests queue behind it.
const CAPTURE_LIMIT: Duration = Duration::from_secs(2);

/// Runs `command`, killing it once `limit` has passed.
pub fn output_within(mut command: Command, limit: Duration) -> anyhow::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain both pipes on threads so a large capture cannot block the child on a full pipe.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("timed out after {} s", limit.as_secs_f32());
        }
        thread::sleep(Duration::from_millis(5));
    };
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// The bottom of a terminal capture, without its trailing blank rows.
pub fn vt(bytes: &[u8]) -> Text<'static> {
    let start = bytes
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(TAIL_LINES)
        .map_or(0, |(index, _)| index + 1);
    let mut text = bytes[start..]
        .to_vec()
        .into_text()
        .unwrap_or_else(|_| Text::raw(String::from_utf8_lossy(&bytes[start..]).into_owned()));
    while text
        .lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
        text.lines.pop();
    }
    text
}

fn dim() -> Style {
    Style::new().fg(Color::Indexed(8))
}

fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let mut out: String = line.trim().chars().take(max).collect();
    if line.trim().chars().count() > max {
        out.push('…');
    }
    out
}

/// The string value of `key` in JSON that may be cut short (Ultimate trims tool args).
fn field(json: &str, key: &str) -> Option<String> {
    let start = json.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut out = String::new();
    let mut chars = json[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push(' '),
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

fn tool_summary(name: &str, args: &str) -> String {
    let home = dirs::home_dir().map(|home| home.to_string_lossy().into_owned());
    let detail = ["command", "path", "url", "text", "action", "code"]
        .iter()
        .find_map(|key| field(args, key))
        .unwrap_or_default();
    let detail = match &home {
        Some(home) => detail.replace(home.as_str(), "~"),
        None => detail,
    };
    format!("{name}  {}", first_line(&detail, 120))
}

/// Who spoke, what was said, and one dim line per tool call. Oldest at the top.
pub fn conversation_text(entries: &[Value]) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut last_speaker = "";
    for entry in entries.iter().rev() {
        let kind = entry
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let role = entry
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = entry
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (speaker, colour) = match (kind, role) {
            (_, "assistant") => ("agent", Color::Indexed(2)),
            ("pi.user", _) => ("you", Color::Indexed(4)),
            (_, "toolResult") | (_, "system") => ("", Color::Reset),
            (_, "user") if !text.is_empty() => ("note", Color::Indexed(5)),
            _ => ("", Color::Reset),
        };
        if speaker.is_empty() {
            if role == "toolResult" && entry.get("error").is_some_and(|error| !error.is_null()) {
                lines.push(Line::styled(
                    format!("  ✕ {}", first_line(text, 100)),
                    Style::new().fg(Color::Indexed(1)),
                ));
            }
            continue;
        }
        if !text.trim().is_empty() {
            if speaker != last_speaker || speaker == "you" {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(Line::styled(
                    speaker.to_owned(),
                    Style::new().fg(colour).add_modifier(Modifier::BOLD),
                ));
            }
            let body = if speaker == "note" {
                vec![first_line(text, 140)]
            } else {
                text.lines().map(str::to_owned).collect()
            };
            for line in body {
                lines.push(Line::raw(line));
            }
            last_speaker = speaker;
        }
        for tool in entry
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
            let args = tool.get("args").and_then(Value::as_str).unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled("  ▸ ", dim()),
                Span::styled(tool_summary(name, args), dim()),
            ]));
            // Text after a tool call names its speaker again.
            last_speaker = "";
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled("nothing said yet", dim()));
    }
    Text::from(lines)
}

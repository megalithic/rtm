//! Jumps, kills and new worktrees. Every process, HTTP and CDP call goes through `Io`,
//! so tests point them at fakes and never touch live Rex sessions or Ultimate instances.
use crate::model::{JumpTarget, Row};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

pub trait Io {
    /// One HTTP request; returns the response body.
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String>;
    /// Runs a program to completion; returns stdout, or stderr as the error.
    fn run(&self, program: &str, args: &[&str], cwd: Option<&str>) -> Result<String>;
}

pub struct System;

impl Io for System {
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(800)))
            .max_redirects(0)
            .build()
            .new_agent();
        let mut response = if method == "POST" {
            let mut request = agent.post(url);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            request.send(body.unwrap_or_default())?
        } else {
            let mut request = agent.get(url);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            request.call()?
        };
        Ok(response.body_mut().read_to_string()?)
    }

    fn run(&self, program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
        let mut command = Command::new(program);
        command.args(args).stdin(Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let output = command.output().with_context(|| format!("run {program}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("{}", first_error(&stdout, &stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// rwt --json prints `{ok:false, error}` on stdout; otherwise the last stderr line says why.
fn first_error(stdout: &str, stderr: &str) -> String {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
        .or_else(|| {
            stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_owned())
        })
        .unwrap_or_else(|| "failed".into())
}

pub fn rex_bin() -> String {
    crate::rex::which_rex()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rex".into())
}

pub fn rwt_bin() -> String {
    dirs::home_dir()
        .map(|home| home.join(".dotfiles/bin/rwt"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("rwt"))
        .to_string_lossy()
        .into_owned()
}

#[derive(Clone, Debug, Deserialize)]
pub struct Tab {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
}

/// The first page whose URL is on `base` (also as `localhost` instead of `127.0.0.1`).
pub fn find_tab<'a>(tabs: &'a [Tab], base: &str) -> Option<&'a Tab> {
    let base = base.trim_end_matches('/');
    let alias = base.replace("://127.0.0.1:", "://localhost:");
    tabs.iter().find(|tab| {
        tab.kind == "page"
            && [base, alias.as_str()].iter().any(|prefix| {
                tab.url
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
            })
    })
}

fn cdp() -> String {
    format!(
        "http://127.0.0.1:{}",
        std::env::var("PIDEWAVE_CDP_PORT").unwrap_or_else(|_| "9223".into())
    )
}

/// Raises the Helium tab already on `base`; with no tab or no CDP, opens `fallback`.
pub fn focus_or_open(io: &dyn Io, base: &str, fallback: &str) -> Result<()> {
    let cdp = cdp();
    let tab = io
        .http("GET", &format!("{cdp}/json/list"), &[], None)
        .ok()
        .and_then(|body| serde_json::from_str::<Vec<Tab>>(&body).ok())
        .and_then(|tabs| find_tab(&tabs, base).cloned());
    match tab {
        Some(tab)
            if io
                .http("GET", &format!("{cdp}/json/activate/{}", tab.id), &[], None)
                .is_ok() =>
        {
            io.run("open", &["-a", "Helium"], None)?;
        }
        _ => {
            io.run("open", &[fallback], None)?;
        }
    }
    Ok(())
}

/// Every open tab of the instance switches to the conversation; then its tab is raised.
pub fn jump_ultimate(io: &dyn Io, url: &str, conversation: u64) -> Result<()> {
    show(
        io,
        url,
        serde_json::json!({"shell.conversation": conversation}),
        &format!("{url}/?conversation={conversation}"),
    )
}

/// Every open tab of the instance switches to its Inbox. A new tab opens on `key`, the oldest
/// entry; an existing tab shows the Inbox's own selection (it is per tab, in the browser).
pub fn jump_inbox(io: &dyn Io, url: &str, key: &str) -> Result<()> {
    show(
        io,
        url,
        serde_json::json!({"shell.surface": "inbox"}),
        &format!("{url}/?inbox={key}"),
    )
}

/// Sets `state` in every open tab of the instance, then raises one; `fallback` opens otherwise.
fn show(io: &dyn Io, url: &str, state: serde_json::Value, fallback: &str) -> Result<()> {
    let body = serde_json::json!({ "set": state }).to_string();
    let switched = io.http(
        "POST",
        &format!("{url}/api/ui"),
        &[("Origin", url), ("Content-Type", "application/json")],
        Some(&body),
    );
    // Without the switch, an existing tab would show the wrong place: open the link.
    if switched.is_err() {
        io.run("open", &[fallback], None)?;
        return Ok(());
    }
    focus_or_open(io, url, fallback)
}

pub fn jump_rex(
    io: &dyn Io,
    client: Option<&str>,
    session: &str,
    window: Option<&str>,
    block: Option<&str>,
) -> Result<()> {
    let rex = rex_bin();
    if let Some(block) = block {
        let args = serde_json::json!({"session_id": session, "block_id": block}).to_string();
        io.run(
            &rex,
            &[
                "--autostart=false",
                "api",
                "call",
                "session.focus_block",
                &args,
            ],
            None,
        )?;
    }
    let session_arg = format!("session_id={session}");
    let window_arg = window.map(|window| format!("window_id={window}"));
    let mut select = vec!["do", "session.select", session_arg.as_str()];
    if let Some(window) = &window_arg {
        select.push(window);
    }
    if let Some(client) = client {
        let mut with_client = vec!["--autostart=false", "-C", client];
        with_client.extend(&select);
        if io.run(&rex, &with_client, None).is_ok() {
            return Ok(());
        }
        // The client that opened the picker can be gone; Rex then uses the app.
    }
    let mut args = vec!["--autostart=false"];
    args.extend(&select);
    io.run(&rex, &args, None)?;
    Ok(())
}

pub fn jump(io: &dyn Io, client: Option<&str>, row: &Row) -> Result<()> {
    match &row.jump {
        JumpTarget::Rex {
            session,
            window,
            block,
        } => jump_rex(io, client, session, window.as_deref(), block.as_deref()),
        JumpTarget::Ultimate {
            url, conversation, ..
        } => jump_ultimate(io, url, *conversation),
        JumpTarget::Inbox { url, key } => jump_inbox(io, url, key),
    }
}

/// `rwt down` stops a session's processes and keeps the worktree.
pub fn kill_args(session: &str) -> Vec<String> {
    ["down", "--session", session, "--json"]
        .map(str::to_owned)
        .to_vec()
}

pub fn kill(io: &dyn Io, session: &str) -> Result<()> {
    let args = kill_args(session);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    io.run(&rwt_bin(), &args, None)?;
    Ok(())
}

/// Killing the session rtm runs in kills rtm too: hand the work to a detached process.
/// Nobody sees its output then, so it goes to `~/.cache/rtm/kill.log`.
pub fn kill_detached(session: &str) -> Result<()> {
    let log = crate::cache::root().and_then(|root| {
        std::fs::create_dir_all(&root)?;
        Ok(std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("kill.log"))?)
    });
    let (stdout, stderr) = match log.and_then(|file| Ok((file.try_clone()?, file))) {
        Ok((out, err)) => (Stdio::from(out), Stdio::from(err)),
        Err(_) => (Stdio::null(), Stdio::null()),
    };
    let mut command = Command::new(rwt_bin());
    command
        .args(kill_args(session))
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().context("start rwt down")?;
    Ok(())
}

pub fn create_args(name: &str, base: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["new", name, "--json", "--show"]
        .map(str::to_owned)
        .to_vec();
    if let Some(base) = base {
        args.extend(["--base".into(), base.into()]);
    }
    args
}

pub fn create(io: &dyn Io, repo: &str, name: &str, base: Option<&str>) -> Result<()> {
    let args = create_args(name, base);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    io.run(&rwt_bin(), &args, Some(repo))?;
    Ok(())
}

/// The branch checked out at `path`; none when detached or not in git.
pub fn current_branch(path: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", path, "branch", "--show-current"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let branch = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !branch.is_empty()).then_some(branch)
}

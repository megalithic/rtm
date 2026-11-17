use crate::{cache, rex};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Worktree {
    pub path: String,
    pub repo: String,
    pub branch: String,
}

pub type Worktrees = BTreeMap<String, Worktree>;
pub type PullRequests = BTreeMap<String, Vec<PullRequest>>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub head_ref_name: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default)]
    pub merge_state_status: String,
    #[serde(default)]
    pub status_check_rollup: Option<Vec<Value>>,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Colour {
    Red,
    Green,
    Blue,
}

impl PullRequest {
    pub fn colour(&self) -> Colour {
        const FAILURES: &[&str] = &[
            "FAILURE",
            "ERROR",
            "CANCELLED",
            "TIMED_OUT",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
        ];
        let failed = self.status_check_rollup.iter().flatten().any(|check| {
            ["conclusion", "state"].iter().any(|field| {
                check
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| FAILURES.contains(&value))
            })
        });
        if self.mergeable == "CONFLICTING" || self.merge_state_status == "DIRTY" || failed {
            Colour::Red
        } else if !self.is_draft && self.merge_state_status == "CLEAN" {
            Colour::Green
        } else {
            Colour::Blue
        }
    }
}

fn git(cwd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    if !output.status.success() {
        bail!(
            "git in {cwd}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub fn worktree(cwd: &str) -> Result<Worktree> {
    let path = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let common = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = Path::new(&common);
    if common.file_name().is_none_or(|name| name != ".git") {
        bail!(
            "unsupported git common directory: {} (expected main checkout/.git)",
            common.display()
        );
    }
    let repo = common
        .parent()
        .context("git common directory parent")?
        .to_string_lossy()
        .into_owned();
    let branch = git(&path, &["branch", "--show-current"])?;
    Ok(Worktree { path, repo, branch })
}

/// How long a cwd's worktree lookup is reused. Each lookup is three git processes, and a
/// long-running `rtm pick` refreshes every few seconds; branches rarely change faster.
const REMEMBER: Duration = Duration::from_secs(30);

type Lookup = std::result::Result<Worktree, String>;

fn remembered(cwd: &str) -> Result<Worktree> {
    static SEEN: Mutex<BTreeMap<String, (Instant, Lookup)>> = Mutex::new(BTreeMap::new());
    if let Some((at, lookup)) = SEEN.lock().ok().and_then(|seen| seen.get(cwd).cloned())
        && at.elapsed() < REMEMBER
    {
        return lookup.map_err(anyhow::Error::msg);
    }
    let lookup = worktree(cwd).map_err(|error| format!("{error:#}"));
    if let Ok(mut seen) = SEEN.lock() {
        seen.insert(cwd.to_owned(), (Instant::now(), lookup.clone()));
    }
    lookup.map_err(anyhow::Error::msg)
}

pub fn discover(sessions: &[rex::Session]) -> (Worktrees, Vec<String>) {
    let cwds: BTreeSet<_> = sessions
        .iter()
        .flat_map(|session| &session.blocks)
        .filter_map(rex::Block::cwd)
        .collect();
    thread::scope(|scope| {
        let workers: Vec<_> = cwds
            .into_iter()
            .map(|cwd| (cwd, scope.spawn(move || remembered(cwd))))
            .collect();
        let mut trees = Worktrees::new();
        let mut errors = Vec::new();
        for (cwd, worker) in workers {
            match worker.join().expect("git worker panicked") {
                Ok(tree) => {
                    trees.insert(cwd.to_owned(), tree);
                }
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        (trees, errors)
    })
}

pub fn pr_path(root: &Path, repo: &str) -> PathBuf {
    // Stable FNV-1a; unlike DefaultHasher, cache names survive Rust upgrades.
    let hash = repo
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    root.join("prs").join(format!("{hash:016x}.json"))
}

pub fn repos(trees: &Worktrees) -> Vec<String> {
    trees
        .values()
        .map(|tree| tree.repo.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn refresh(root: &Path, repo: &str) -> Result<Vec<PullRequest>> {
    let path = pr_path(root, repo);
    if cache::fresh(&path, Duration::from_secs(60))
        && let Ok(Some(prs)) = cache::read(&path)
    {
        return Ok(prs);
    }
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,headRefName,isDraft,mergeable,mergeStateStatus,statusCheckRollup,url",
            "--limit",
            "100",
        ])
        .current_dir(repo)
        .output()
        .with_context(|| format!("gh pr list in {repo}"))?;
    if !output.status.success() {
        bail!(
            "gh pr list in {repo}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let prs: Vec<PullRequest> = serde_json::from_slice(&output.stdout)?;
    cache::write(&path, &prs)?;
    Ok(prs)
}

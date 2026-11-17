use crate::{
    cache, git,
    model::{self, Model},
    now_ms,
    registry::{self, Registry},
    rex, seen, ultimate,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub model: Model,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub timings_ms: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Refresh {
    pub at: u64,
    pub timings_ms: BTreeMap<String, f64>,
    pub errors: Vec<String>,
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    (f(), start.elapsed().as_secs_f64() * 1000.0)
}

fn cached<T: serde::de::DeserializeOwned + Default>(path: &Path, warnings: &mut Vec<String>) -> T {
    match cache::read(path) {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            warnings.push(format!("{error:#}"));
            T::default()
        }
    }
}

/// Also used by the short-lived background subprocess, so a CLI exit cannot abandon refresh threads.
pub fn refresh(root: &Path, repos: &[String]) -> Result<Refresh> {
    let mut report = Refresh {
        at: now_ms(),
        ..Refresh::default()
    };
    thread::scope(|scope| {
        let registry = scope.spawn(|| timed(|| registry::refresh(root)));
        let workers: Vec<_> = repos
            .iter()
            .map(|repo| {
                (
                    repo,
                    scope.spawn(move || timed(|| git::refresh(root, repo))),
                )
            })
            .collect();
        let (result, elapsed) = registry.join().expect("registry worker panicked");
        report.timings_ms.insert("registry".into(), elapsed);
        if let Err(error) = result {
            report.errors.push(format!("registry: {error:#}"));
        }
        for (repo, worker) in workers {
            let (result, elapsed) = worker.join().expect("PR worker panicked");
            report.timings_ms.insert(format!("prs:{repo}"), elapsed);
            if let Err(error) = result {
                report.errors.push(format!("{error:#}"));
            }
        }
    });
    cache::write(&root.join("refresh.json"), &report)?;
    Ok(report)
}

fn start_refresh(root: &Path, repos: &[String]) -> Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("__refresh")
        .arg(root)
        .args(repos)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn background cache refresh")?;
    // Reap in a long-lived caller (the future TUI) without blocking a one-shot CLI.
    thread::spawn(move || child.wait());
    Ok(())
}

pub fn load(wait: bool) -> Result<Snapshot> {
    let start = Instant::now();
    let mut timings = BTreeMap::new();
    let mut warnings = Vec::new();
    let (rex_result, ultimate_result) = thread::scope(|scope| {
        let rex = scope.spawn(|| timed(rex::load));
        let ultimate = scope.spawn(|| timed(ultimate::load));
        (
            rex.join().expect("Rex worker panicked"),
            ultimate.join().expect("Ultimate worker panicked"),
        )
    });
    timings.insert("rex".into(), rex_result.1);
    timings.insert("ultimate".into(), ultimate_result.1);
    timings.insert(
        "rex+ultimate_wall".into(),
        start.elapsed().as_secs_f64() * 1000.0,
    );
    let sessions = match rex_result.0 {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("{error:#}"));
            Vec::new()
        }
    };
    let instances = match ultimate_result.0 {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("{error:#}"));
            Vec::new()
        }
    };
    let ((trees, errors), elapsed) = timed(|| git::discover(&sessions));
    timings.insert("git".into(), elapsed);
    warnings.extend(errors);
    let root = cache::root()?;
    let repos = git::repos(&trees);
    if wait {
        let (report, elapsed) = timed(|| refresh(&root, &repos));
        timings.insert("refresh_wall".into(), elapsed);
        let report = report?;
        warnings.extend(report.errors);
        for (name, ms) in report.timings_ms {
            timings.insert(format!("refresh:{name}"), ms);
        }
    } else {
        if let Ok(Some(report)) = cache::read::<Refresh>(&root.join("refresh.json")) {
            warnings.extend(report.errors);
        }
        // `rtm pick` loads every few seconds; the registry and PRs need not refresh as often.
        if !cache::fresh(&root.join("refresh.json"), Duration::from_secs(10))
            && let Err(error) = start_refresh(&root, &repos)
        {
            warnings.push(format!("{error:#}"));
        }
    }
    let (registry, elapsed) = timed(|| cached::<Registry>(&registry::path(&root), &mut warnings));
    timings.insert("registry_cache".into(), elapsed);
    let (prs, elapsed) = timed(|| {
        repos
            .iter()
            .map(|repo| {
                (
                    repo.clone(),
                    cached(&git::pr_path(&root, repo), &mut warnings),
                )
            })
            .collect()
    });
    timings.insert("pr_cache".into(), elapsed);
    let (seen, elapsed) = timed(|| seen::Store::open(seen::path()?));
    timings.insert("seen".into(), elapsed);
    let seen = seen?;
    let (model, elapsed) = timed(|| {
        model::build(&model::Inputs {
            rex: &sessions,
            ultimates: &instances,
            registry: &registry,
            worktrees: &trees,
            prs: &prs,
            seen: seen.values(),
            now: now_ms(),
        })
    });
    timings.insert("model".into(), elapsed);
    timings.insert("total".into(), start.elapsed().as_secs_f64() * 1000.0);
    Ok(Snapshot {
        model,
        warnings,
        timings_ms: timings,
    })
}

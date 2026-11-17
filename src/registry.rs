use crate::cache;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Registry {
    #[serde(default)]
    pub apps: Vec<App>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct App {
    pub port: u16,
    pub origin: Option<String>,
    pub root: Option<String>,
    /// The registry keeps apps whose server has stopped; they are never shown.
    #[serde(default)]
    pub dead: bool,
}

pub fn path(root: &Path) -> PathBuf {
    root.join("registry.json")
}

pub fn refresh(root: &Path) -> Result<Registry> {
    let program = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("home directory"))?
        .join(".dotfiles/bin/agents");
    let output = Command::new(program).arg("--json").output()?;
    if !output.status.success() {
        bail!("agents --json: {}", String::from_utf8_lossy(&output.stderr));
    }
    let registry = serde_json::from_slice::<Registry>(&output.stdout)?;
    // Preserve the complete source document for inspection and future adapters.
    cache::write(
        &path(root),
        &serde_json::from_slice::<serde_json::Value>(&output.stdout)?,
    )?;
    Ok(registry)
}

/// An app can cover several nested worktrees; the deepest app root wins per cwd.
/// A dead deepest app hides the cwd's app rather than falling back to a parent's.
pub fn apps_for<'a>(registry: &'a Registry, cwds: &[&str]) -> Vec<&'a App> {
    let mut apps = Vec::new();
    for cwd in cwds {
        let selected = registry
            .apps
            .iter()
            .filter(|app| {
                app.root
                    .as_ref()
                    .is_some_and(|root| Path::new(cwd).starts_with(root))
            })
            .max_by_key(|app| app.root.as_ref().map_or(0, String::len));
        if let Some(app) = selected
            && !app.dead
            && !apps.iter().any(|existing: &&App| existing.port == app.port)
        {
            apps.push(app);
        }
    }
    apps.sort_by_key(|app| app.port);
    apps
}

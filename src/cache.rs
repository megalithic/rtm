use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub fn root() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or(dirs::home_dir().context("home directory")?.join(".cache"))
        .join("rtm"))
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

pub fn fresh(path: &Path, ttl: Duration) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age < ttl)
}

/// Readers see either the complete old document or the complete new one.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().context("cache parent directory")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".rtm-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

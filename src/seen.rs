use crate::{cache, now_ms};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

pub type Seen = BTreeMap<String, u64>;

pub fn path() -> Result<PathBuf> {
    Ok(std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::home_dir()
                .context("home directory")?
                .join(".local/state"),
        )
        .join("rtm/seen.json"))
}

pub struct Store {
    path: PathBuf,
    values: Seen,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let values = cache::read(&path)?.unwrap_or_default();
        Ok(Self { path, values })
    }

    pub fn get(&self, key: &str) -> u64 {
        self.values.get(key).copied().unwrap_or_default()
    }

    pub fn values(&self) -> &Seen {
        &self.values
    }

    pub fn mark(&mut self, key: &str) -> Result<()> {
        self.mark_at(key, now_ms())
    }

    pub fn mark_at(&mut self, key: &str, time: u64) -> Result<()> {
        fs::create_dir_all(self.path.parent().context("seen parent directory")?)?;
        // Lock a stable sibling, not the inode replaced by the atomic rename.
        // Closing it releases the advisory lock, including on an error path.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut values: Seen = cache::read(&self.path)?.unwrap_or_default();
        let previous = values.get(key).copied().unwrap_or_default();
        values.insert(key.to_owned(), time.max(previous));
        cache::write(&self.path, &values)?;
        self.values = values;
        Ok(())
    }
}

pub use alice::Alice;
pub use bob::Bob;
pub use cipher::StateCipher;
pub use sqlite::{format_history_date, SqliteDatabase};

use crate::fs::ensure_directory_exists;
use crate::protocol::{Database, State};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod alice;
mod bob;
mod cipher;
mod sqlite;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum Swap {
    Alice(Alice),
    Bob(Bob),
}

impl From<State> for Swap {
    fn from(state: State) -> Self {
        match state {
            State::Alice(state) => Swap::Alice(state.into()),
            State::Bob(state) => Swap::Bob(state.into()),
        }
    }
}

impl From<Swap> for State {
    fn from(value: Swap) -> Self {
        match value {
            Swap::Alice(alice) => State::Alice(alice.into()),
            Swap::Bob(bob) => State::Bob(bob.into()),
        }
    }
}

impl From<Alice> for Swap {
    fn from(from: Alice) -> Self {
        Swap::Alice(from)
    }
}

impl From<Bob> for Swap {
    fn from(from: Bob) -> Self {
        Swap::Bob(from)
    }
}

impl Display for Swap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Swap::Alice(alice) => Display::fmt(alice, f),
            Swap::Bob(bob) => Display::fmt(bob, f),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq)]
#[error("Not in the role of Alice")]
struct NotAlice;

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq)]
#[error("Not in the role of Bob")]
struct NotBob;

impl Swap {
    pub fn try_into_alice(self) -> Result<Alice> {
        match self {
            Swap::Alice(alice) => Ok(alice),
            Swap::Bob(_) => bail!(NotAlice),
        }
    }

    pub fn try_into_bob(self) -> Result<Bob> {
        match self {
            Swap::Bob(bob) => Ok(bob),
            Swap::Alice(_) => bail!(NotBob),
        }
    }
}

pub async fn open_db(sqlite_path: impl AsRef<Path>) -> Result<Arc<dyn Database + Send + Sync>> {
    open_db_with_cipher(sqlite_path, None).await
}

pub async fn open_db_with_cipher(
    sqlite_path: impl AsRef<Path>,
    cipher: Option<StateCipher>,
) -> Result<Arc<dyn Database + Send + Sync>> {
    let path = sqlite_path.as_ref();
    if path.exists() {
        tracing::debug!("Using existing sqlite database.");
        let sqlite = SqliteDatabase::open_with_cipher(path, cipher).await?;
        restrict_database_permissions(path)?;
        Ok(Arc::new(sqlite))
    } else {
        tracing::debug!("Creating and using new sqlite database.");
        ensure_directory_exists(path)?;
        tokio::fs::File::create(path).await?;
        restrict_database_permissions(path)?;
        let sqlite = SqliteDatabase::open_with_cipher(path, cipher).await?;
        Ok(Arc::new(sqlite))
    }
}

fn restrict_database_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{}", path.display(), suffix));
            if candidate.exists() {
                std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    Ok(())
}

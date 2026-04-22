use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemhubError {
    #[error("no memhub project above {start}; run `memhub init`")]
    NotInitialized { start: PathBuf },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid managed markdown in {path}: {reason}")]
    InvalidManagedMarkdown { path: String, reason: String },
    #[error("{command} failed: {stderr}")]
    ExternalCommand { command: String, stderr: String },
    #[error("feature not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    TomlDeserialize(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),
}

pub type Result<T> = std::result::Result<T, MemhubError>;

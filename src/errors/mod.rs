use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemhubError {
    #[error("no memhub project above {start}; run `memhub init`")]
    NotInitialized { start: PathBuf },
    #[error(
        "memhub database missing at {db_path} but {memhub_dir} exists; \
         this is a recovery case. Run `memhub init --from-backup <path>` to restore \
         from a memhub export, or remove {memhub_dir} to start over."
    )]
    MissingDatabase {
        memhub_dir: PathBuf,
        db_path: PathBuf,
    },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid managed markdown in {path}: {reason}")]
    InvalidManagedMarkdown { path: String, reason: String },
    #[error("{command} failed: {stderr}")]
    ExternalCommand { command: String, stderr: String },
    #[error("mcp error: {0}")]
    Mcp(String),
    #[error("embedding error: {0}")]
    Embedding(String),
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
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, MemhubError>;

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
    #[error("{command} failed: {stderr}")]
    ExternalCommand { command: String, stderr: String },
    #[error("mcp error: {0}")]
    Mcp(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("rerank error: {0}")]
    Rerank(String),
    #[error("feature not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(rusqlite::Error),
    // D9: SQLITE_BUSY / SQLITE_LOCKED (the 5s `busy_timeout` was exceeded,
    // almost always because another memhub process is mid-write) surfaces
    // through MCP and the CLI as a raw rusqlite string otherwise. Mapped
    // here to an actionable message instead; every other rusqlite error
    // still comes through `Sqlite` unchanged. Only ever constructed by the
    // `From<rusqlite::Error>` impl below.
    #[error("memhub database is busy — another memhub process may be writing. Retry in a moment.")]
    DatabaseBusy {
        #[source]
        source: rusqlite::Error,
    },
    #[error(transparent)]
    TomlDeserialize(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

// Hand-written instead of `#[from]` so SQLITE_BUSY / SQLITE_LOCKED (D9) can
// be special-cased into `DatabaseBusy`; every other rusqlite error still
// wraps into `Sqlite` unchanged.
impl From<rusqlite::Error> for MemhubError {
    fn from(err: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(ref ffi_err, _) = err {
            if matches!(
                ffi_err.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) {
                return MemhubError::DatabaseBusy { source: err };
            }
        }
        MemhubError::Sqlite(err)
    }
}

pub type Result<T> = std::result::Result<T, MemhubError>;

#[cfg(test)]
mod tests {
    use super::*;

    const FRIENDLY_BUSY_MESSAGE: &str =
        "memhub database is busy — another memhub process may be writing. Retry in a moment.";

    fn sqlite_failure(sqlite_result_code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(sqlite_result_code),
            Some("simulated failure".to_string()),
        )
    }

    #[test]
    fn sqlite_busy_maps_to_friendly_message() {
        let err: MemhubError = sqlite_failure(rusqlite::ffi::SQLITE_BUSY).into();
        assert_eq!(err.to_string(), FRIENDLY_BUSY_MESSAGE);
        assert!(matches!(err, MemhubError::DatabaseBusy { .. }));
    }

    #[test]
    fn sqlite_locked_maps_to_friendly_message() {
        let err: MemhubError = sqlite_failure(rusqlite::ffi::SQLITE_LOCKED).into();
        assert_eq!(err.to_string(), FRIENDLY_BUSY_MESSAGE);
        assert!(matches!(err, MemhubError::DatabaseBusy { .. }));
    }

    #[test]
    fn other_sqlite_failure_passes_through_unchanged() {
        // A non-busy SqliteFailure (e.g. a constraint violation) must keep
        // surfacing its original rusqlite message, not the friendly one.
        let inner = sqlite_failure(rusqlite::ffi::SQLITE_CONSTRAINT);
        let expected_message = inner.to_string();
        let err: MemhubError = inner.into();
        assert_eq!(err.to_string(), expected_message);
        assert!(matches!(err, MemhubError::Sqlite(_)));
    }

    #[test]
    fn non_failure_rusqlite_errors_pass_through_unchanged() {
        // A rusqlite error that isn't even a `SqliteFailure` variant (no
        // ffi error code at all) must be untouched by the busy mapping.
        let inner = rusqlite::Error::QueryReturnedNoRows;
        let expected_message = inner.to_string();
        let err: MemhubError = inner.into();
        assert_eq!(err.to_string(), expected_message);
        assert!(matches!(err, MemhubError::Sqlite(_)));
    }
}

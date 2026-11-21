use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Action, Category, Item, Subject, identifier, timestamp};
use crate::commands::sync::sha256_file;
use crate::db::{MEMHUB_DIR, ProjectPaths};
use crate::fs_identity::{FileIdentity, file_identity, file_info, is_link_or_reparse_point};
use crate::{MemhubError, Result};

#[cfg(windows)]
mod windows;

pub(crate) const RETAIN: usize = 20;
const SAFETY_SLOTS: [&str; 3] = ["sync", "import", "migrations"];
#[cfg(not(windows))]
pub(super) const REPORT_ONLY: &str = "Backup cleanup is report-only on this platform.";

#[derive(PartialEq, Eq)]
struct Backup {
    path: PathBuf,
    identity: FileIdentity,
    modified: SystemTime,
    bytes: u64,
    hash: String,
}

pub(super) struct Group {
    id: String,
    directory: PathBuf,
    #[cfg(windows)]
    directory_identity: FileIdentity,
    files: Vec<Backup>,
}

impl Group {
    pub(super) fn item(self) -> Item {
        let stale = &self.files[RETAIN..];
        let id = self.id;
        #[cfg(windows)]
        let unavailable = windows::check_storage(&self.directory)
            .err()
            .map(|e| format!("Backup cleanup unavailable: {e}"));
        #[cfg(not(windows))]
        let unavailable = Some(REPORT_ONLY.to_owned());
        let mut actions = vec![Action::keep(&id)];
        let mut reason = format!("{} backup file(s) beyond the newest {RETAIN}.", stale.len());
        if let Some(unavailable) = unavailable {
            reason.push(' ');
            reason.push_str(&unavailable);
        } else {
            actions.push(Action::new(
                "delete",
                ["prune", "backups", "--yes", "--candidate", &id],
            ));
        }
        Item {
            id,
            category: Category::SurplusBackups,
            subjects: vec![Subject::BackupFiles {
                directory: self.directory.display().to_string(),
                files: stale
                    .iter()
                    .map(|file| file.path.display().to_string())
                    .collect(),
            }],
            reason,
            actions,
        }
    }
}

pub(super) fn surplus(paths: &ProjectPaths) -> Result<Vec<Group>> {
    let root = fs::canonicalize(&paths.repo_root)?;
    let memhub = root.join(MEMHUB_DIR);
    require_directory(&memhub)?;
    let backups = memhub.join("backups");
    match fs::symlink_metadata(&backups) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(at_path(&backups, err)),
        Ok(_) => require_directory(&backups)?,
    }

    let mut groups = Vec::new();
    let mut dirs = vec![backups.clone()];
    while let Some(dir) = dirs.pop() {
        require_directory(&dir)?;
        let directory_identity = file_identity(&dir).map_err(|err| at_path(&dir, err))?;
        let mut files = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|err| at_path(&dir, err))? {
            let entry = entry.map_err(|err| at_path(&dir, err))?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|err| at_path(&path, err))?;
            if is_link_or_reparse_point(&meta) {
                continue;
            }
            if meta.is_dir() {
                if dir != backups
                    || !SAFETY_SLOTS
                        .iter()
                        .any(|name| entry.file_name().eq_ignore_ascii_case(name))
                {
                    dirs.push(path);
                }
            } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "bak") {
                require_direct(&path)?;
                let info = file_info(&path).map_err(|err| at_path(&path, err))?;
                if info.links == 1 {
                    let modified = meta.modified().map_err(|err| at_path(&path, err))?;
                    files.push((path, info.identity, modified, meta.len()));
                }
            }
        }
        if files.len() <= RETAIN {
            continue;
        }
        // Match gc's newest-by-mtime rule, with a stable filename tie break.
        files.sort_by(|a, b| (Reverse(a.2), &a.0).cmp(&(Reverse(b.2), &b.0)));
        // Hash content, not just metadata. An in-place edit with a retained
        // size and mtime must invalidate the reviewed batch.
        let files = files
            .into_iter()
            .map(|(path, identity, modified, bytes)| -> Result<_> {
                require_direct(&path)?;
                let hash = sha256_file(&path)?;
                Ok(Backup {
                    path,
                    identity,
                    modified,
                    bytes,
                    hash,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let relative = dir.strip_prefix(&root).expect("walk under repo root");
        let fingerprint: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    f.path.file_name().expect("entry name").as_encoded_bytes(),
                    &f.identity,
                    timestamp(f.modified),
                    f.bytes,
                    &f.hash,
                )
            })
            .collect();
        let id = identifier(
            Category::SurplusBackups,
            (
                root.as_os_str().as_encoded_bytes(),
                relative.as_os_str().as_encoded_bytes(),
                &directory_identity,
                fingerprint,
            ),
        )?;
        groups.push(Group {
            id,
            directory: dir,
            #[cfg(windows)]
            directory_identity,
            files,
        });
    }
    groups.sort_by(|a, b| a.directory.cmp(&b.directory));
    Ok(groups)
}

#[cfg(windows)]
pub(super) fn delete(paths: &ProjectPaths, candidate: &str) -> Result<usize> {
    // Do not open the database: even ordinary maintained opens can modify the
    // DB, replace the migration backup, and expire proposals.
    let memhub = fs::canonicalize(&paths.repo_root)?.join(MEMHUB_DIR);
    require_directory(&memhub)?;
    let database = memhub.join(crate::db::DB_FILENAME);
    require_direct(&database)?;
    if !fs::symlink_metadata(&database)?.is_file() {
        return Err(MemhubError::InvalidInput(format!(
            "not a memhub database: {}",
            database.display()
        )));
    }
    let group = surplus(paths)?
        .into_iter()
        .find(|group| group.id == candidate)
        .ok_or_else(|| {
            MemhubError::InvalidInput(format!(
                "no current surplus backup candidate '{candidate}'; run `memhub prune scan` again"
            ))
        })?;
    windows::delete(group)
}

fn require_direct(path: &Path) -> Result<()> {
    if is_link_or_reparse_point(&fs::symlink_metadata(path).map_err(|err| at_path(path, err))?)
        || fs::canonicalize(path).map_err(|err| at_path(path, err))? != path
    {
        return Err(changed(path));
    }
    Ok(())
}

fn require_directory(path: &Path) -> Result<()> {
    require_direct(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|err| at_path(path, err))?
        .is_dir()
    {
        return Err(changed(path));
    }
    Ok(())
}

fn changed(path: &Path) -> MemhubError {
    MemhubError::InvalidInput(format!(
        "backup path '{}' changed or is not a direct path; refusing cleanup",
        path.display()
    ))
}

fn at_path(path: &Path, err: io::Error) -> MemhubError {
    io::Error::new(
        err.kind(),
        format!("backup path '{}': {err}", path.display()),
    )
    .into()
}

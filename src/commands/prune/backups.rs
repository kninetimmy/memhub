use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Action, Category, Item, Subject, identifier};
use crate::commands::sync::sha256_file;
use crate::db::{DB_FILENAME, MEMHUB_DIR, ProjectPaths};
use crate::fs_identity::{FileIdentity, file_identity, file_info};
use crate::{MemhubError, Result};

pub(crate) const RETAIN: usize = 20;
const SAFETY_SLOTS: [&str; 3] = ["sync", "import", "migrations"];

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
    directory_identity: FileIdentity,
    files: Vec<Backup>,
}

impl Group {
    pub(super) fn item(self) -> Item {
        let stale = &self.files[RETAIN..];
        let id = self.id;
        let actions = vec![
            Action::new("delete", ["prune", "backups", "--yes", "--candidate", &id]),
            Action::keep(&id),
        ];
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
            reason: format!("{} backup file(s) beyond the newest {RETAIN}.", stale.len()),
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
            if linked(&meta) {
                continue;
            }
            if meta.is_dir() {
                if dir != backups || !SAFETY_SLOTS.iter().any(|name| *name == entry.file_name()) {
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
                    f.modified,
                    f.bytes,
                    &f.hash,
                )
            })
            .collect();
        let id = identifier(
            Category::SurplusBackups,
            (relative.as_os_str().as_encoded_bytes(), fingerprint),
        )?;
        groups.push(Group {
            id,
            directory: dir,
            directory_identity,
            files,
        });
    }
    groups.sort_by(|a, b| a.directory.cmp(&b.directory));
    Ok(groups)
}

pub(super) fn delete(paths: &ProjectPaths, candidate: Option<&str>) -> Result<usize> {
    // Do not open the database: even ordinary maintained opens can modify the
    // DB, replace the migration backup, and expire proposals.
    let memhub = fs::canonicalize(&paths.repo_root)?.join(MEMHUB_DIR);
    require_directory(&memhub)?;
    let database = memhub.join(DB_FILENAME);
    require_direct(&database)?;
    if !fs::symlink_metadata(&database)?.is_file() {
        return Err(MemhubError::InvalidInput(format!(
            "not a memhub database: {}",
            database.display()
        )));
    }
    let mut groups = surplus(paths)?;
    if let Some(id) = candidate {
        groups.retain(|group| group.id == id);
        if groups.is_empty() {
            return Err(MemhubError::InvalidInput(format!(
                "no current surplus backup candidate '{id}'; run `memhub prune scan` again"
            )));
        }
    }

    let mut removed = 0;
    for group in groups {
        for file in &group.files[RETAIN..] {
            // Revalidate both the directory and the selected file before
            // unlinking. An edited/replaced entry is not the approved copy.
            validate_group(&group)?;
            require_direct(&file.path)?;
            let meta = fs::symlink_metadata(&file.path).map_err(|err| at_path(&file.path, err))?;
            let info = file_info(&file.path).map_err(|err| at_path(&file.path, err))?;
            if linked(&meta)
                || !meta.is_file()
                || info.links != 1
                || info.identity != file.identity
                || meta.modified()? != file.modified
                || meta.len() != file.bytes
                || sha256_file(&file.path)? != file.hash
            {
                return Err(changed(&file.path));
            }
            validate_group(&group)?;
            fs::remove_file(&file.path).map_err(|err| at_path(&file.path, err))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn validate_group(group: &Group) -> Result<()> {
    require_directory(&group.directory)?;
    if file_identity(&group.directory)? != group.directory_identity {
        return Err(changed(&group.directory));
    }
    Ok(())
}

fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other reparse points, not just symlinks.
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn require_direct(path: &Path) -> Result<()> {
    if linked(&fs::symlink_metadata(path).map_err(|err| at_path(path, err))?)
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

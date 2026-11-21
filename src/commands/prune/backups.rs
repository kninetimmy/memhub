use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Action, Category, Item, Subject, identifier, timestamp};
use crate::commands::sync::sha256_reader;
use crate::db::{MEMHUB_DIR, ProjectPaths};
use crate::fs_identity::{
    FileIdentity, direct_subdirectory, file_identity, file_info, file_info_from_handle,
    is_link_or_reparse_point, open_direct_file,
};
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
    #[cfg(windows)]
    cleanup_eligibility: (u32, bool),
}

pub(super) struct Group {
    id: String,
    directory: PathBuf,
    #[cfg(windows)]
    directory_identity: FileIdentity,
    #[cfg(windows)]
    cleanup_blocker: Option<MemhubError>,
    files: Vec<Backup>,
}

impl Group {
    pub(super) fn item(self) -> Item {
        let stale = &self.files[RETAIN..];
        #[cfg(windows)]
        let unavailable = self
            .cleanup_blocker
            .map(|e| format!("Backup cleanup unavailable: {e}"));
        #[cfg(not(windows))]
        let unavailable = Some(REPORT_ONLY.to_owned());
        let id = self.id;
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
                directory: display_path(&self.directory),
                files: stale.iter().map(|file| display_path(&file.path)).collect(),
            }],
            reason,
            actions,
        }
    }
}

fn display_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::fmt::Write;
        use std::os::unix::ffi::OsStrExt;

        // Preserve UTF-8, but distinguish literal backslashes from escapes.
        let mut escaped = String::new();
        for chunk in path.as_os_str().as_bytes().utf8_chunks() {
            for c in chunk.valid().chars() {
                if c == '\\' || c.is_control() {
                    escaped.extend(c.escape_default());
                } else {
                    escaped.push(c);
                }
            }
            for byte in chunk.invalid() {
                write!(escaped, "\\x{byte:02x}").expect("write to string");
            }
        }
        escaped
    }
    #[cfg(windows)]
    {
        use std::fmt::Write;
        use std::os::windows::ffi::OsStrExt;

        // Leave path separators intact. Doubling a literal '%' reserves
        // %uNNNN for otherwise unprintable native UTF-16 units.
        let mut escaped = String::new();
        for unit in char::decode_utf16(path.as_os_str().encode_wide()) {
            match unit {
                Ok('%') => escaped.push_str("%%"),
                Ok(c) if c.is_control() => {
                    write!(escaped, "%u{:04x}", u32::from(c)).expect("write to string");
                }
                Ok(c) => escaped.push(c),
                Err(e) => {
                    write!(escaped, "%u{:04x}", e.unpaired_surrogate()).expect("write to string");
                }
            }
        }
        escaped
    }
    #[cfg(not(any(unix, windows)))]
    path.display().to_string()
}

pub(super) fn surplus(
    paths: &ProjectPaths,
    removed: &[PathBuf],
) -> Result<(Vec<Group>, Vec<String>)> {
    let root = fs::canonicalize(&paths.repo_root).map_err(|err| at_path(&paths.repo_root, err))?;
    let memhub = root.join(MEMHUB_DIR);
    require_directory(&root, &memhub)?;
    let backups = memhub.join("backups");
    match fs::symlink_metadata(&backups) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(err) => return Err(at_path(&backups, err)),
        Ok(_) => require_directory(&root, &backups)?,
    }

    let mut groups = Vec::new();
    let mut warnings = Vec::new();
    let mut dirs = vec![backups.clone()];
    while let Some(dir) = dirs.pop() {
        match inspect_directory(&root, &backups, &dir, removed, &mut dirs) {
            Ok(Some(group)) => groups.push(group),
            Ok(None) => {}
            Err(err) => warnings.push(format!("'{}': {err}", display_path(&dir))),
        }
    }
    groups.sort_by(|a, b| a.directory.cmp(&b.directory));
    warnings.sort();
    Ok((groups, warnings))
}

fn inspect_directory(
    root: &Path,
    backups: &Path,
    dir: &Path,
    removed: &[PathBuf],
    dirs: &mut Vec<PathBuf>,
) -> Result<Option<Group>> {
    require_directory(root, dir)?;
    let directory_identity = file_identity(dir).map_err(|err| at_path(dir, err))?;
    let mut files = Vec::new();
    let mut error = None;
    for entry in fs::read_dir(dir).map_err(|err| at_path(dir, err))? {
        let (entry, meta) = match entry
            .and_then(|entry| fs::symlink_metadata(entry.path()).map(|meta| (entry, meta)))
        {
            Ok(inspected) => inspected,
            Err(err) => {
                // Continue discovering independently inspectable children,
                // but do not derive a candidate from this partial listing.
                error.get_or_insert_with(|| at_path(dir, err));
                continue;
            }
        };
        let path = entry.path();
        if removed.iter().any(|removed| path.starts_with(removed)) {
            continue;
        }
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
            files.push((path, meta));
        }
    }
    if let Some(err) = error {
        return Err(err);
    }
    if files.len() <= RETAIN {
        return Ok(None);
    }
    let mut eligible = Vec::new();
    for (path, meta) in files {
        require_direct(&path)?;
        let info = file_info(&path).map_err(|err| at_path(&path, err))?;
        if info.links == 1 {
            let modified = meta.modified().map_err(|err| at_path(&path, err))?;
            eligible.push((path, info, modified, meta));
        }
    }
    if eligible.len() <= RETAIN {
        return Ok(None);
    }
    // Match gc's newest-by-mtime rule, with a stable filename tie break.
    eligible.sort_by(|a, b| (Reverse(a.2), &a.0).cmp(&(Reverse(b.2), &b.0)));
    // Hash content, not just metadata. An in-place edit with a retained
    // size and mtime must invalidate the reviewed batch.
    #[cfg(windows)]
    let mut cleanup_blocker = None;
    let files = eligible
        .into_iter()
        .enumerate()
        .map(|(index, (path, info, modified, meta))| -> Result<_> {
            #[cfg(not(windows))]
            let _ = index;
            require_direct(&path)?;
            let file = open_direct_file(&path, &meta).map_err(|err| at_path(&path, err))?;
            let actual = file.metadata().map_err(|err| at_path(&path, err))?;
            if file_info_from_handle(&file).map_err(|err| at_path(&path, err))? != info
                || actual.modified().map_err(|err| at_path(&path, err))? != modified
                || actual.len() != meta.len()
            {
                return Err(changed(&path));
            }
            #[cfg(windows)]
            let cleanup_eligibility = {
                use std::os::windows::fs::MetadataExt;
                let attributes = actual.file_attributes();
                let check = windows::check_file(&file, attributes, &path, index >= RETAIN);
                let eligible = check.is_ok();
                cleanup_blocker = cleanup_blocker.take().or_else(|| check.err());
                (attributes & windows::CLEANUP_ATTRIBUTE_MASK, eligible)
            };
            let hash = sha256_reader(&file).map_err(|err| {
                MemhubError::InvalidInput(format!(
                    "cannot hash backup '{}': {err}",
                    display_path(&path)
                ))
            })?;
            Ok(Backup {
                path,
                identity: info.identity,
                modified,
                bytes: meta.len(),
                hash,
                #[cfg(windows)]
                cleanup_eligibility,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let relative = dir.strip_prefix(root).expect("walk under repo root");
    let fingerprint: Vec<_> = files
        .iter()
        .map(|f| {
            let state = (
                f.path.file_name().expect("entry name").as_encoded_bytes(),
                &f.identity,
                timestamp(f.modified),
                f.bytes,
                &f.hash,
            );
            #[cfg(windows)]
            let state = (state, f.cleanup_eligibility);
            state
        })
        .collect();
    let state = (
        root.as_os_str().as_encoded_bytes(),
        relative.as_os_str().as_encoded_bytes(),
        &directory_identity,
        fingerprint,
    );
    #[cfg(windows)]
    let state = {
        let storage = windows::storage_path(dir, &directory_identity);
        let supported = storage.is_ok();
        cleanup_blocker = cleanup_blocker.or_else(|| storage.err());
        (state, supported)
    };
    let id = identifier(Category::SurplusBackups, state)?;
    Ok(Some(Group {
        id,
        directory: dir.to_path_buf(),
        #[cfg(windows)]
        directory_identity,
        #[cfg(windows)]
        cleanup_blocker,
        files,
    }))
}

#[cfg(windows)]
pub(super) fn delete(paths: &ProjectPaths, candidate: &str) -> Result<usize> {
    // Do not open the database: even ordinary maintained opens can modify the
    // DB, replace the migration backup, and expire proposals.
    let root = fs::canonicalize(&paths.repo_root)?;
    let memhub = root.join(MEMHUB_DIR);
    require_directory(&root, &memhub)?;
    let database = memhub.join(crate::db::DB_FILENAME);
    require_direct(&database)?;
    if !fs::symlink_metadata(&database)?.is_file() {
        return Err(MemhubError::InvalidInput(format!(
            "not a memhub database: {}",
            database.display()
        )));
    }
    let (groups, warnings) = surplus(paths, &[])?;
    let group = groups
        .into_iter()
        .find(|group| group.id == candidate)
        .ok_or_else(|| {
            let detail = warning(warnings).map_or_else(String::new, |w| format!("; {w}"));
            MemhubError::InvalidInput(format!(
                "no current surplus backup candidate '{candidate}'; run `memhub prune scan` again{detail}"
            ))
        })?;
    windows::delete(group)
}

pub(super) fn warning(warnings: Vec<String>) -> Option<String> {
    (!warnings.is_empty()).then(|| format!("Surplus backups not counted: {}", warnings.join("; ")))
}

fn require_direct(path: &Path) -> Result<()> {
    if is_link_or_reparse_point(&fs::symlink_metadata(path).map_err(|err| at_path(path, err))?)
        || fs::canonicalize(path).map_err(|err| at_path(path, err))? != path
    {
        return Err(changed(path));
    }
    Ok(())
}

fn require_directory(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| changed(path))?;
    direct_subdirectory(root, relative)
        .map(|_| ())
        .map_err(|err| at_path(path, err))
}

fn changed(path: &Path) -> MemhubError {
    MemhubError::InvalidInput(format!(
        "backup path '{}' changed or is not a direct path; refusing cleanup",
        display_path(path)
    ))
}

fn at_path(path: &Path, err: io::Error) -> MemhubError {
    io::Error::new(
        err.kind(),
        format!("backup path '{}': {err}", display_path(path)),
    )
    .into()
}

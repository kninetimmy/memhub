use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rmcp::schemars;
use serde::Serialize;

use crate::code_index::validate_code_index_storage;
use crate::db::{CONFIG_EXAMPLE_FILENAME, CONFIG_FILENAME, DB_FILENAME, MEMHUB_DIR};
use crate::fs_identity::{FileIdentity, file_identity, file_info};
use crate::{MemhubError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum BindingMode {
    Discovered,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum RepositoryRelationship {
    SameCheckout,
    RegisteredWorktree,
    MemoryOnly,
}

/// Launch-fixed project and checkout evidence returned by the project-aware
/// MCP read tools. Paths are canonical so aliases cannot create a second
/// authorization identity.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(super) struct BindingEvidence {
    pub mode: BindingMode,
    pub memory_root: String,
    pub code_root: String,
    pub repository_relationship: RepositoryRelationship,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ServerBinding {
    mode: BindingMode,
    memory_root: PinnedPath,
    code_root: PinnedPath,
    memory_store: PinnedMemoryStore,
    git: Option<PinnedGitPair>,
}

impl ServerBinding {
    pub(super) fn resolve(
        start: &Path,
        memory_root: Option<&Path>,
        code_root: Option<&Path>,
    ) -> Result<Self> {
        match (memory_root, code_root) {
            (Some(memory_root), Some(code_root)) => Self::explicit(memory_root, code_root),
            (None, None) => Self::discovered(start),
            _ => Err(binding_error(
                "--memory-root and --code-root must be supplied together",
            )),
        }
    }

    fn explicit(memory_root: &Path, code_root: &Path) -> Result<Self> {
        let memory_root = canonical_directory("memory root", memory_root)?;
        let code_root = canonical_directory("code root", code_root)?;
        let memory_store = PinnedMemoryStore::capture(&memory_root)?;
        validate_code_index_storage(&code_root, false)?;

        let memory_checkout = GitCheckout::require("memory root", &memory_root)?;
        let code_checkout = GitCheckout::require("code root", &code_root)?;
        require_checkout_root("memory root", &memory_root, &memory_checkout)?;
        require_checkout_root("code root", &code_root, &code_checkout)?;
        let git = PinnedGitPair::capture(memory_checkout, code_checkout)?;

        Ok(Self {
            mode: BindingMode::Explicit,
            memory_root: PinnedPath::capture("memory root", memory_root)?,
            code_root: PinnedPath::capture("code root", code_root)?,
            memory_store,
            git: Some(git),
        })
    }

    fn discovered(start: &Path) -> Result<Self> {
        let paths = crate::db::discover_paths(start)?;
        let memory_root = canonical_directory("discovered memory root", &paths.repo_root)?;
        let launch_dir = canonical_directory("launch directory", start)?;
        let memory_store = PinnedMemoryStore::capture(&memory_root)?;
        validate_code_index_storage(&memory_root, false)?;

        let memory_checkout = GitCheckout::inspect(&memory_root)?;
        let launch_checkout = GitCheckout::inspect(&launch_dir)?;
        let git = match (memory_checkout, launch_checkout) {
            (None, None) => None,
            (Some(memory_checkout), Some(launch_checkout)) => {
                require_checkout_root("memory root", &memory_root, &memory_checkout)?;
                if launch_checkout.root != memory_root {
                    return Err(binding_error(format!(
                        "launch directory resolves to Git checkout '{}' but memory discovery \
                         resolves to '{}'; refusing to return ancestor-checkout code. Launch \
                         with paired --memory-root and --code-root arguments",
                        launch_checkout.root.display(),
                        memory_root.display()
                    )));
                }
                Some(PinnedGitPair::capture(memory_checkout, launch_checkout)?)
            }
            (Some(_), None) => {
                return Err(binding_error(
                    "the discovered memory root is a Git checkout but the launch directory \
                     could not be verified as that checkout",
                ));
            }
            (None, Some(checkout)) => {
                return Err(binding_error(format!(
                    "launch directory is inside Git checkout '{}' but the discovered memory \
                     root is not that checkout; use paired --memory-root and --code-root \
                     arguments",
                    checkout.root.display()
                )));
            }
        };

        Ok(Self {
            mode: BindingMode::Discovered,
            memory_root: PinnedPath::capture("memory root", memory_root.clone())?,
            code_root: PinnedPath::capture("code root", memory_root)?,
            memory_store,
            git,
        })
    }

    pub(super) fn memory_root(&self) -> &Path {
        &self.memory_root.path
    }

    pub(super) fn code_root(&self) -> &Path {
        &self.code_root.path
    }

    pub(super) fn config_path(&self) -> PathBuf {
        self.memory_store.metadata_dir.path.join(CONFIG_FILENAME)
    }

    pub(super) fn evidence(&self) -> BindingEvidence {
        let (relationship, git_common_dir) = match &self.git {
            Some(git) => (
                git.relationship,
                Some(git.common_dir.path.display().to_string()),
            ),
            None => (RepositoryRelationship::MemoryOnly, None),
        };
        BindingEvidence {
            mode: self.mode,
            memory_root: self.memory_root.path.display().to_string(),
            code_root: self.code_root.path.display().to_string(),
            repository_relationship: relationship,
            git_common_dir,
        }
    }

    /// Revalidate both pinned roots and their Git registration before every
    /// tool call. This deliberately does not rediscover `.memhub`: a cache
    /// directory appearing in a worktree cannot change the launch binding.
    pub(super) fn revalidate(&self) -> Result<()> {
        self.memory_root.revalidate()?;
        self.code_root.revalidate()?;
        self.memory_store.revalidate(&self.memory_root.path)?;
        validate_code_index_storage(&self.code_root.path, false)?;

        match &self.git {
            Some(git) => git.revalidate(&self.memory_root.path, &self.code_root.path),
            None => {
                if GitCheckout::inspect(&self.memory_root.path)?.is_some() {
                    return Err(binding_error(
                        "the launch-bound memory-only root became a Git checkout",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
struct PinnedGitPair {
    memory_git_dir: PinnedPath,
    code_git_dir: PinnedPath,
    common_dir: PinnedPath,
    relationship: RepositoryRelationship,
}

impl PinnedGitPair {
    fn capture(memory: GitCheckout, code: GitCheckout) -> Result<Self> {
        if memory.common_dir != code.common_dir {
            return Err(binding_error(format!(
                "memory root '{}' and code root '{}' are unrelated Git repositories",
                memory.root.display(),
                code.root.display()
            )));
        }
        require_registered_worktrees(&memory.root, &memory.root, &code.root)?;
        let relationship = if memory.root == code.root {
            RepositoryRelationship::SameCheckout
        } else {
            RepositoryRelationship::RegisteredWorktree
        };
        Ok(Self {
            memory_git_dir: PinnedPath::capture("memory Git directory", memory.git_dir)?,
            code_git_dir: PinnedPath::capture("code Git directory", code.git_dir)?,
            common_dir: PinnedPath::capture("Git common directory", memory.common_dir)?,
            relationship,
        })
    }

    fn revalidate(&self, memory_root: &Path, code_root: &Path) -> Result<()> {
        self.memory_git_dir.revalidate()?;
        self.code_git_dir.revalidate()?;
        self.common_dir.revalidate()?;

        let memory = GitCheckout::require("memory root", memory_root)?;
        let code = GitCheckout::require("code root", code_root)?;
        require_checkout_root("memory root", memory_root, &memory)?;
        require_checkout_root("code root", code_root, &code)?;
        if memory.git_dir != self.memory_git_dir.path
            || code.git_dir != self.code_git_dir.path
            || memory.common_dir != self.common_dir.path
            || code.common_dir != self.common_dir.path
        {
            return Err(binding_error(
                "the bound checkout or its Git repository identity changed after launch",
            ));
        }
        let relationship = if memory.root == code.root {
            RepositoryRelationship::SameCheckout
        } else {
            RepositoryRelationship::RegisteredWorktree
        };
        if relationship != self.relationship {
            return Err(binding_error(
                "the verified repository relationship changed after launch",
            ));
        }
        require_registered_worktrees(memory_root, memory_root, code_root)
    }
}

#[derive(Debug, Clone)]
struct GitCheckout {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl GitCheckout {
    fn inspect(path: &Path) -> Result<Option<Self>> {
        let top = git(path, &["rev-parse", "--show-toplevel"])?;
        if !top.status.success() {
            let detail = String::from_utf8_lossy(&top.stderr);
            if detail.contains("not a git repository") {
                return Ok(None);
            }
            return Err(git_failure(path, "rev-parse --show-toplevel", &top.stderr));
        }
        let root = canonical_git_path("Git checkout root", &top.stdout)?;
        let dirs = git(
            path,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
        )?;
        if !dirs.status.success() {
            return Err(git_failure(path, "rev-parse Git directories", &dirs.stderr));
        }
        let mut lines = dirs.stdout.split(|byte| *byte == b'\n');
        let git_dir = canonical_git_path(
            "Git directory",
            lines
                .next()
                .ok_or_else(|| binding_error("Git did not report its directory"))?,
        )?;
        let common_dir = canonical_git_path(
            "Git common directory",
            lines
                .next()
                .ok_or_else(|| binding_error("Git did not report its common directory"))?,
        )?;
        Ok(Some(Self {
            root,
            git_dir,
            common_dir,
        }))
    }

    fn require(label: &str, path: &Path) -> Result<Self> {
        Self::inspect(path)?.ok_or_else(|| {
            binding_error(format!(
                "{label} '{}' is not a verifiable Git checkout",
                path.display()
            ))
        })
    }
}

fn require_checkout_root(label: &str, supplied: &Path, checkout: &GitCheckout) -> Result<()> {
    if checkout.root != supplied {
        return Err(binding_error(format!(
            "{label} '{}' is not the checkout root Git reports ('{}')",
            supplied.display(),
            checkout.root.display()
        )));
    }
    Ok(())
}

fn require_registered_worktrees(repo: &Path, memory_root: &Path, code_root: &Path) -> Result<()> {
    let output = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    if !output.status.success() {
        return Err(git_failure(
            repo,
            "worktree list --porcelain -z",
            &output.stderr,
        ));
    }
    let mut roots = Vec::new();
    for field in output.stdout.split(|byte| *byte == 0) {
        let Some(raw) = field.strip_prefix(b"worktree ") else {
            continue;
        };
        let text = std::str::from_utf8(raw).map_err(|_| {
            binding_error("Git reported a non-UTF-8 worktree path; binding is unverifiable")
        })?;
        if let Ok(root) = fs::canonicalize(text) {
            roots.push(root);
        }
    }
    for (label, expected) in [("memory root", memory_root), ("code root", code_root)] {
        if !roots.iter().any(|root| root == expected) {
            return Err(binding_error(format!(
                "{label} '{}' is not a currently registered Git worktree",
                expected.display()
            )));
        }
    }
    Ok(())
}

fn git(path: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| {
            binding_error(format!(
                "could not verify Git binding at '{}': {error}",
                path.display()
            ))
        })
}

fn git_failure(path: &Path, operation: &str, stderr: &[u8]) -> MemhubError {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    binding_error(format!(
        "Git {operation} failed at '{}': {}",
        path.display(),
        if detail.is_empty() {
            "no error detail"
        } else {
            &detail
        }
    ))
}

fn canonical_git_path(label: &str, raw: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| binding_error(format!("{label} is not valid UTF-8")))?
        .trim_end_matches(['\r', '\n']);
    canonical_directory(label, Path::new(text))
}

fn canonical_directory(label: &str, path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        binding_error(format!(
            "{label} '{}' cannot be resolved: {error}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(binding_error(format!(
            "{label} '{}' is not a directory",
            path.display()
        )));
    }
    Ok(canonical)
}

#[derive(Debug, Clone)]
struct PinnedMemoryStore {
    metadata_dir: PinnedPath,
}

impl PinnedMemoryStore {
    fn capture(memory_root: &Path) -> Result<Self> {
        let metadata_dir = validate_memory_store(memory_root)?;
        Ok(Self {
            metadata_dir: PinnedPath::capture("memory metadata directory", metadata_dir)?,
        })
    }

    fn revalidate(&self, memory_root: &Path) -> Result<()> {
        let current = validate_memory_store(memory_root)?;
        if current != self.metadata_dir.path {
            return Err(binding_error(
                "memory metadata directory changed after server launch",
            ));
        }
        self.metadata_dir.revalidate()?;
        Ok(())
    }
}

fn validate_memory_store(memory_root: &Path) -> Result<PathBuf> {
    let metadata_dir = memory_root.join(MEMHUB_DIR);
    let metadata = fs::symlink_metadata(&metadata_dir).map_err(|error| {
        memory_storage_error(format!(
            "cannot inspect metadata directory '{}': {error}",
            metadata_dir.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(memory_storage_error(format!(
            "metadata directory '{}' is not a direct directory",
            metadata_dir.display()
        )));
    }
    let actual_dir = fs::canonicalize(&metadata_dir).map_err(|error| {
        memory_storage_error(format!(
            "cannot resolve metadata directory '{}': {error}",
            metadata_dir.display()
        ))
    })?;
    if actual_dir != metadata_dir {
        return Err(memory_storage_error(format!(
            "metadata directory '{}' resolves outside the fixed memory root to '{}'",
            metadata_dir.display(),
            actual_dir.display()
        )));
    }

    let db_path = metadata_dir.join(DB_FILENAME);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        validate_direct_memory_file(&sqlite_path(&db_path, suffix), suffix.is_empty())?;
    }
    validate_direct_memory_file(&metadata_dir.join(CONFIG_FILENAME), false)?;
    validate_direct_memory_file(&metadata_dir.join(CONFIG_EXAMPLE_FILENAME), false)?;
    Ok(metadata_dir)
}

fn validate_direct_memory_file(path: &Path, required: bool) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => return Ok(()),
        Err(error) => {
            return Err(memory_storage_error(format!(
                "cannot inspect '{}': {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(memory_storage_error(format!(
            "'{}' is not a direct regular file",
            path.display()
        )));
    }
    let actual = fs::canonicalize(path).map_err(|error| {
        memory_storage_error(format!("cannot resolve '{}': {error}", path.display()))
    })?;
    if actual != path {
        return Err(memory_storage_error(format!(
            "'{}' resolves to '{}'",
            path.display(),
            actual.display()
        )));
    }
    let info = file_info(path).map_err(|error| {
        memory_storage_error(format!("cannot verify '{}': {error}", path.display()))
    })?;
    if info.links != 1 {
        return Err(memory_storage_error(format!(
            "'{}' has {} hard links; primary memory storage must not be aliased",
            path.display(),
            info.links
        )));
    }
    Ok(())
}

fn sqlite_path(db_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return db_path.to_path_buf();
    }
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn memory_storage_error(message: impl Into<String>) -> MemhubError {
    binding_error(format!("memory storage invalid: {}", message.into()))
}

#[derive(Debug, Clone)]
struct PinnedPath {
    label: &'static str,
    path: PathBuf,
    identity: FileIdentity,
}

impl PinnedPath {
    fn capture(label: &'static str, path: PathBuf) -> Result<Self> {
        let identity = file_identity(&path).map_err(|error| {
            binding_error(format!(
                "cannot read bound path identity '{}': {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            label,
            path,
            identity,
        })
    }

    fn revalidate(&self) -> Result<()> {
        let current = canonical_directory(self.label, &self.path)?;
        let identity = file_identity(&current).map_err(|error| {
            binding_error(format!(
                "cannot read bound path identity '{}': {error}",
                current.display()
            ))
        })?;
        if current != self.path || identity != self.identity {
            return Err(binding_error(format!(
                "{} '{}' was replaced after server launch",
                self.label,
                self.path.display()
            )));
        }
        Ok(())
    }
}

fn binding_error(message: impl Into<String>) -> MemhubError {
    MemhubError::Binding(message.into())
}

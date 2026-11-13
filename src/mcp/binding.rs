use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rmcp::schemars;
use serde::Serialize;

use crate::db::{DB_FILENAME, MEMHUB_DIR, ProjectPaths};
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
        require_memory_store(&memory_root)?;

        let memory_checkout = GitCheckout::require("memory root", &memory_root)?;
        let code_checkout = GitCheckout::require("code root", &code_root)?;
        require_checkout_root("memory root", &memory_root, &memory_checkout)?;
        require_checkout_root("code root", &code_root, &code_checkout)?;
        let git = PinnedGitPair::capture(memory_checkout, code_checkout)?;

        Ok(Self {
            mode: BindingMode::Explicit,
            memory_root: PinnedPath::capture("memory root", memory_root)?,
            code_root: PinnedPath::capture("code root", code_root)?,
            git: Some(git),
        })
    }

    fn discovered(start: &Path) -> Result<Self> {
        let paths = crate::db::discover_paths(start)?;
        let memory_root = canonical_directory("discovered memory root", &paths.repo_root)?;
        let launch_dir = canonical_directory("launch directory", start)?;
        require_memory_store(&memory_root)?;

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
        self.memory_root
            .path
            .join(MEMHUB_DIR)
            .join(crate::db::CONFIG_FILENAME)
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
        require_memory_store(&self.memory_root.path)?;

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

fn require_memory_store(memory_root: &Path) -> Result<()> {
    let paths = ProjectPaths::for_repo_root(memory_root);
    if !paths.memhub_dir.is_dir() {
        return Err(binding_error(format!(
            "memory root '{}' has no {MEMHUB_DIR} project metadata directory",
            memory_root.display()
        )));
    }
    if !paths.db_path.is_file() {
        return Err(binding_error(format!(
            "memory root '{}' has no {MEMHUB_DIR}/{DB_FILENAME}",
            memory_root.display()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    first: u64,
    second: u64,
}

impl FileIdentity {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path).map_err(|error| {
            binding_error(format!(
                "cannot read bound path identity '{}': {error}",
                path.display()
            ))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                first: metadata.dev(),
                second: metadata.ino(),
            })
        }

        #[cfg(windows)]
        {
            let _ = metadata;
            let (volume, index) = windows_file_identity(path)?;
            Ok(Self {
                first: u64::from(volume),
                second: index,
            })
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = metadata;
            Err(binding_error(format!(
                "filesystem identity verification is unsupported for '{}'",
                path.display()
            )))
        }
    }
}

#[cfg(windows)]
fn windows_file_identity(path: &Path) -> Result<(u32, u64)> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    type Handle = *mut c_void;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        fn GetFileInformationByHandle(
            file: Handle,
            information: *mut ByHandleFileInformation,
        ) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    struct OwnedHandle(Handle);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the live handle returned by CreateFileW
            // and this owner closes it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and remains alive for the call; all
    // optional pointer arguments are null and the constants follow the
    // CreateFileW contract for opening either a file or directory metadata.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(binding_error(format!(
            "cannot open bound path identity '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    let handle = OwnedHandle(raw);
    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `handle` is valid and `information` points to writable storage
    // of the exact BY_HANDLE_FILE_INFORMATION layout.
    if unsafe { GetFileInformationByHandle(handle.0, information.as_mut_ptr()) } == 0 {
        return Err(binding_error(format!(
            "cannot query bound path identity '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: GetFileInformationByHandle succeeded and initialized every
    // field of the output structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    if information.volume_serial_number == 0 && index == 0 {
        return Err(binding_error(format!(
            "filesystem identity for '{}' is unavailable",
            path.display()
        )));
    }
    Ok((information.volume_serial_number, index))
}

#[derive(Debug, Clone)]
struct PinnedPath {
    label: &'static str,
    path: PathBuf,
    identity: FileIdentity,
}

impl PinnedPath {
    fn capture(label: &'static str, path: PathBuf) -> Result<Self> {
        let identity = FileIdentity::read(&path)?;
        Ok(Self {
            label,
            path,
            identity,
        })
    }

    fn revalidate(&self) -> Result<()> {
        let current = canonical_directory(self.label, &self.path)?;
        if current != self.path || FileIdentity::read(&current)? != self.identity {
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

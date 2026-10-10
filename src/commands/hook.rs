//! Session-start hook (issue #286).
//!
//! `memhub hook session-start` (hidden) prints the current repo's rendered
//! `PROJECT.md` to stdout so an agent CLI's SessionStart hook can put the
//! frame into the session without the agent having to obey a
//! "read PROJECT.md" instruction. It must never break a session: on any
//! problem it prints nothing and exits 0, and it never opens or migrates
//! the DB or writes a file.
//!
//! `memhub upgrade` installs the user-scope hook entry that runs it into
//! `~/.claude/settings.json` and `~/.codex/hooks.json`. These files belong
//! to other tools, so the install is a conservative merge: a file that is
//! not valid JSON (or not the expected shape) is left byte-for-byte
//! unchanged, an existing memhub entry is never duplicated, the write is
//! atomic, and an entry the user deleted after memhub added it is not
//! re-added (remembered in `~/.memhub/installed-hooks.json`).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::ProjectConfig;
use crate::db;

/// The command every installed hook handler runs.
pub const SESSION_START_COMMAND: &str = "memhub hook session-start";
const HOOK_TIMEOUT_SECS: u64 = 30;
/// Files (exact path strings) memhub has added its entry to, so a later
/// upgrade can tell "never installed" from "the user removed it".
const INSTALLED_FILENAME: &str = "installed-hooks.json";
const CODEX_APPROVAL_NOTE: &str =
    "approve it once in Codex's /hooks screen; Codex skips a new hook until you do";

/// The rendered PROJECT.md bytes for the memhub repo containing `start`,
/// or `None` outside a repo / when the config or the file cannot be read.
/// Read-only: discovery plus two file reads, no DB open.
pub fn session_start_output(start: &Path) -> Option<Vec<u8>> {
    let paths = db::discover_paths(start).ok()?;
    let config = ProjectConfig::load(&paths.config_path).ok()?;
    std::fs::read(
        paths
            .repo_root
            .join(&config.render.output_dir)
            .join(crate::render::PROJECT_FILENAME),
    )
    .ok()
}

/// `memhub hook session-start`. Every failure (including a closed stdout)
/// is swallowed: the caller exits 0 regardless.
pub fn session_start() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    if let Some(bytes) = session_start_output(&cwd) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&bytes);
        let _ = out.flush();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    Added,
    /// `--dry-run`: a real run would add the entry.
    WouldAdd,
    AlreadyPresent,
    /// memhub added it before and the user has since removed it.
    LeftOut,
    /// The file could not be read, parsed, or written; left unchanged.
    NotInstalled,
    /// `--no-hooks`.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookInstall {
    pub agent: String,
    /// `~`-relative display path of the file.
    pub file: String,
    pub status: HookStatus,
    pub detail: Option<String>,
}

impl HookInstall {
    pub fn line(&self) -> String {
        let what = match self.status {
            HookStatus::Added => "added SessionStart entry",
            HookStatus::WouldAdd => "would add SessionStart entry",
            HookStatus::AlreadyPresent => "SessionStart entry already present",
            HookStatus::LeftOut => "left out",
            HookStatus::NotInstalled => "not installed",
            HookStatus::Skipped => "skipped",
        };
        match &self.detail {
            Some(d) => format!("{:<7} {}: {what} ({d})", self.agent, self.file),
            None => format!("{:<7} {}: {what}", self.agent, self.file),
        }
    }
}

/// (agent, path under home, matcher). Claude Code's `clear`/`compact`
/// sources have no Codex equivalent.
const TARGETS: [(&str, [&str; 2], &str); 2] = [
    (
        "claude",
        [".claude", "settings.json"],
        "startup|resume|clear|compact",
    ),
    ("codex", [".codex", "hooks.json"], "startup|resume"),
];

fn label(rel: [&str; 2]) -> String {
    format!("~/{}/{}", rel[0], rel[1])
}

/// The `--no-hooks` report: both files, untouched.
pub fn skipped_all(reason: &str) -> Vec<HookInstall> {
    TARGETS
        .iter()
        .map(|(agent, rel, _)| HookInstall {
            agent: agent.to_string(),
            file: label(*rel),
            status: HookStatus::Skipped,
            detail: Some(reason.to_string()),
        })
        .collect()
}

/// Merge memhub's SessionStart entry into both user files. Best-effort and
/// never fatal: every problem becomes a `NotInstalled` row. `dry` reports
/// without writing anything.
pub fn install_session_hooks(dry: bool) -> Vec<HookInstall> {
    let home = match db::home_dir() {
        Ok(h) => h,
        Err(e) => {
            return TARGETS
                .iter()
                .map(|(agent, rel, _)| HookInstall {
                    agent: agent.to_string(),
                    file: label(*rel),
                    status: HookStatus::NotInstalled,
                    detail: Some(e.to_string()),
                })
                .collect();
        }
    };
    let marker = home
        .join(db::GLOBAL_MEMHUB_DIRNAME)
        .join(INSTALLED_FILENAME);
    // Absent or corrupt => empty: nothing reads as user-removed.
    let before: BTreeSet<String> = std::fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut installed = before.clone();

    let reports = TARGETS
        .iter()
        .map(|(agent, rel, matcher)| {
            let path = home.join(rel[0]).join(rel[1]);
            let (status, mut detail) = install_one(&path, matcher, &mut installed, dry);
            if *agent == "codex" && matches!(status, HookStatus::Added | HookStatus::WouldAdd) {
                detail = Some(CODEX_APPROVAL_NOTE.to_string());
            }
            HookInstall {
                agent: agent.to_string(),
                file: label(*rel),
                status,
                detail,
            }
        })
        .collect();

    if !dry
        && installed != before
        && let Ok(bytes) = serde_json::to_vec_pretty(&installed)
        && let Err(e) = write_atomic(&marker, &bytes)
    {
        log::debug!("installed-hooks record save skipped: {e}");
    }
    reports
}

fn install_one(
    path: &Path,
    matcher: &str,
    installed: &mut BTreeSet<String>,
    dry: bool,
) -> (HookStatus, Option<String>) {
    let key = path.to_string_lossy().into_owned();
    let mut root = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(e) => {
                return (
                    HookStatus::NotInstalled,
                    Some(format!("not valid JSON ({e}); file left unchanged")),
                );
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => {
            return (
                HookStatus::NotInstalled,
                Some(format!("unreadable ({e}); file left unchanged")),
            );
        }
    };
    let groups = match session_start_groups(&mut root) {
        Ok(g) => g,
        Err(why) => {
            return (
                HookStatus::NotInstalled,
                Some(format!("{why}; file left unchanged")),
            );
        }
    };
    if groups.iter().any(runs_memhub) {
        if !dry {
            installed.insert(key);
        }
        return (HookStatus::AlreadyPresent, None);
    }
    if installed.contains(&key) {
        return (
            HookStatus::LeftOut,
            Some(
                "memhub's entry was removed after an earlier upgrade added it; not re-added"
                    .to_string(),
            ),
        );
    }
    groups.push(json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": SESSION_START_COMMAND,
            "timeout": HOOK_TIMEOUT_SECS,
        }],
    }));
    if dry {
        return (HookStatus::WouldAdd, None);
    }
    let written = serde_json::to_string_pretty(&root)
        .map_err(std::io::Error::from)
        .and_then(|text| write_atomic(path, format!("{text}\n").as_bytes()));
    match written {
        Ok(()) => {
            installed.insert(key);
            (HookStatus::Added, None)
        }
        Err(e) => (
            HookStatus::NotInstalled,
            Some(format!("write failed ({e}); file left unchanged")),
        ),
    }
}

/// `root.hooks.SessionStart`, created if absent. Any present level of the
/// wrong type is an error: the file is not ours to reshape.
fn session_start_groups(root: &mut Value) -> std::result::Result<&mut Vec<Value>, &'static str> {
    root.as_object_mut()
        .ok_or("top level is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" is not a JSON object")?
        .entry("SessionStart")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("\"hooks.SessionStart\" is not a JSON array")
}

fn runs_memhub(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|handlers| {
            handlers.iter().any(|h| {
                h.get("command").and_then(Value::as_str).map(str::trim)
                    == Some(SESSION_START_COMMAND)
            })
        })
}

/// Temp file in the target's own directory, then rename. A symlinked file
/// is written through to its target so the link itself survives, and the
/// original's permissions carry over.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let real: PathBuf = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_name = real.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".memhub-tmp");
    let tmp = real.with_file_name(tmp_name);
    std::fs::write(&tmp, bytes)?;
    if let Ok(meta) = std::fs::metadata(&real) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &real).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

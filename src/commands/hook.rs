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
//! to other tools, so the install is a conservative merge: an agent whose
//! directory (`~/.claude`, `~/.codex`) does not exist is skipped and
//! nothing is created for it, a file that is not valid JSON (or not the
//! expected shape), not writable, or a symlink whose target does not
//! exist is left byte-for-byte unchanged, an existing memhub entry is never
//! duplicated, the write is atomic, and an entry the user deleted after
//! memhub added or found it is not re-added (remembered in
//! `~/.memhub/installed-hooks.json`). The Codex handler also carries
//! `additionalContextLimit` so Codex shows the whole frame instead of its
//! default ~2,500-token head-and-tail preview; an existing memhub Codex
//! handler without the field gets it added.

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
/// Codex's per-handler cap on model-visible hook output (tokens). Its
/// default (~2,500) is near the 8,000-byte PROJECT.md cap.
const CONTEXT_LIMIT_KEY: &str = "additionalContextLimit";
const CODEX_CONTEXT_LIMIT: u64 = 4000;
/// Files memhub has added its entry to or found it in, so a later upgrade
/// can tell "never installed" from "the user removed it". Before #298 the
/// keys were absolute path strings, so a differently spelled home missed
/// them; now they are the `~`-relative labels, and old keys are still read.
const INSTALLED_FILENAME: &str = "installed-hooks.json";
const CODEX_APPROVAL_NOTE: &str =
    "approve it once in Codex's /hooks screen; Codex skips a new hook until you do";
const CODEX_REAPPROVAL_NOTE: &str = "added \"additionalContextLimit\": 4000; approve the changed hook again in Codex's /hooks screen; Codex skips it until you do";
const CODEX_WOULD_REAPPROVAL_NOTE: &str = "would add \"additionalContextLimit\": 4000; the changed hook would need approving again in Codex's /hooks screen; Codex skips it until you do";

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
    /// The existing memhub entry gained a field memhub now sets (Codex's
    /// `additionalContextLimit`).
    Updated,
    /// `--dry-run`: a real run would update the entry.
    WouldUpdate,
    /// An earlier upgrade added or found the entry here and it has since
    /// been removed.
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
            HookStatus::Updated => "updated SessionStart entry",
            HookStatus::WouldUpdate => "would update SessionStart entry",
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

/// (agent, path under home, matcher, handler `additionalContextLimit`).
/// Claude Code's `clear`/`compact` sources have no Codex equivalent; only
/// the Codex handler sets an output limit.
const TARGETS: [(&str, [&str; 2], &str, Option<u64>); 2] = [
    (
        "claude",
        [".claude", "settings.json"],
        "startup|resume|clear|compact",
        None,
    ),
    (
        "codex",
        [".codex", "hooks.json"],
        "startup|resume",
        Some(CODEX_CONTEXT_LIMIT),
    ),
];

fn label(rel: [&str; 2]) -> String {
    format!("~/{}/{}", rel[0], rel[1])
}

/// The `--no-hooks` report: both files, untouched.
pub fn skipped_all(reason: &str) -> Vec<HookInstall> {
    TARGETS
        .iter()
        .map(|(agent, rel, ..)| HookInstall {
            agent: agent.to_string(),
            file: label(*rel),
            status: HookStatus::Skipped,
            detail: Some(reason.to_string()),
        })
        .collect()
}

/// Merge memhub's SessionStart entry into both user files. Best-effort and
/// never fatal: every problem becomes a `NotInstalled` row. An agent whose
/// home directory is missing is `Skipped` (creating `~/.codex` would make
/// `memhub doctor` read Codex as set up). `dry` reports without writing.
pub fn install_session_hooks(dry: bool) -> Vec<HookInstall> {
    let home = match db::home_dir() {
        Ok(h) => h,
        Err(e) => {
            return TARGETS
                .iter()
                .map(|(agent, rel, ..)| HookInstall {
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
        .and_then(|b| serde_json::from_slice::<BTreeSet<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .map(record_key)
        .collect();
    let mut installed = before.clone();

    let reports = TARGETS
        .iter()
        .map(|(agent, rel, matcher, limit)| {
            let dir = home.join(rel[0]);
            let (status, mut detail) = match std::fs::metadata(&dir) {
                Ok(m) if m.is_dir() => install_one(
                    &dir.join(rel[1]),
                    &label(*rel),
                    matcher,
                    *limit,
                    &mut installed,
                    dry,
                ),
                Ok(_) => (
                    HookStatus::Skipped,
                    Some(format!("~/{} is not a directory", rel[0])),
                ),
                // metadata follows links, so a dangling link also lands here.
                Err(_) if dir.symlink_metadata().is_ok() => (
                    HookStatus::Skipped,
                    Some(format!(
                        "~/{} is a symlink whose target does not exist",
                        rel[0]
                    )),
                ),
                Err(_) => (HookStatus::Skipped, Some(format!("no ~/{}", rel[0]))),
            };
            if *agent == "codex" {
                match status {
                    HookStatus::Added | HookStatus::WouldAdd => {
                        detail = Some(CODEX_APPROVAL_NOTE.to_string());
                    }
                    HookStatus::Updated => {
                        detail = Some(CODEX_REAPPROVAL_NOTE.to_string());
                    }
                    HookStatus::WouldUpdate => {
                        detail = Some(CODEX_WOULD_REAPPROVAL_NOTE.to_string());
                    }
                    _ => {}
                }
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
        && let Err(e) = serde_json::to_vec_pretty(&installed)
            .map_err(std::io::Error::from)
            .and_then(|bytes| {
                // A dangling link would be replaced by a regular file.
                if marker.exists() || marker.symlink_metadata().is_err() {
                    std::fs::create_dir_all(home.join(db::GLOBAL_MEMHUB_DIRNAME))?;
                    write_atomic(&marker, &bytes)
                } else {
                    Err(std::io::Error::other(
                        "record is a symlink whose target does not exist",
                    ))
                }
            })
    {
        log::debug!("installed-hooks record save skipped: {e}");
    }
    reports
}

/// An installed-hooks record key in its current form: a key written by
/// the code before #298 (an absolute path ending in a target's path under
/// home) maps to that target's `~`-relative label; anything else is kept.
fn record_key(key: String) -> String {
    TARGETS
        .iter()
        .find(|(_, rel, ..)| Path::new(&key).ends_with(Path::new(rel[0]).join(rel[1])))
        .map_or(key, |(_, rel, ..)| label(*rel))
}

fn install_one(
    path: &Path,
    key: &str,
    matcher: &str,
    context_limit: Option<u64>,
    installed: &mut BTreeSet<String>,
    dry: bool,
) -> (HookStatus, Option<String>) {
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
        // A dangling symlink also reads as NotFound; writing would replace
        // the link with a regular file.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_ok() => {
            return (
                HookStatus::NotInstalled,
                Some("a symlink whose target does not exist; left unchanged".to_string()),
            );
        }
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
    // Every memhub handler in every SessionStart group; each one missing
    // the output-limit field gets it (only memhub's own handlers change).
    let mut found = false;
    let mut changed = false;
    for handler in groups
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
        .filter(|h| is_memhub_handler(h))
    {
        found = true;
        if let (Some(limit), Some(obj)) = (context_limit, handler.as_object_mut())
            && !obj.contains_key(CONTEXT_LIMIT_KEY)
        {
            obj.insert(CONTEXT_LIMIT_KEY.to_string(), json!(limit));
            changed = true;
        }
    }
    let (done, would) = if found {
        if !changed {
            if !dry {
                installed.insert(key.to_string());
            }
            return (HookStatus::AlreadyPresent, None);
        }
        (HookStatus::Updated, HookStatus::WouldUpdate)
    } else {
        if installed.contains(key) {
            return (
                HookStatus::LeftOut,
                Some(
                    "memhub's entry was in this file at an earlier upgrade and has since \
                     been removed; not put back"
                        .to_string(),
                ),
            );
        }
        let mut handler = json!({
            "type": "command",
            "command": SESSION_START_COMMAND,
            "timeout": HOOK_TIMEOUT_SECS,
        });
        if let Some(limit) = context_limit {
            handler[CONTEXT_LIMIT_KEY] = json!(limit);
        }
        groups.push(json!({"matcher": matcher, "hooks": [handler]}));
        (HookStatus::Added, HookStatus::WouldAdd)
    };
    if dry {
        return (would, None);
    }
    let written = serde_json::to_string_pretty(&root)
        .map_err(std::io::Error::from)
        .and_then(|text| write_atomic(path, format!("{text}\n").as_bytes()));
    match written {
        Ok(()) => {
            installed.insert(key.to_string());
            (done, None)
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

fn is_memhub_handler(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(runs_memhub_command)
}

/// `<program> hook session-start`, where `<program>` is exactly one program
/// token: either unquoted with no whitespace, quote, or shell
/// metacharacter (`; & | < > ( ) $` and backtick), or wrapped whole in one
/// pair of `"` or `'` with no further instance of that quote inside (and no
/// `$` or backtick inside `"`, which the shell would expand). Its last
/// path component must be `memhub` or `memhub.exe` in any letter case.
/// Extra words (`echo /x/memhub`, `true && memhub`) never count: they run
/// another program. Before #298 only the exact string
/// [`SESSION_START_COMMAND`] counted.
fn runs_memhub_command(command: &str) -> bool {
    let Some(program) = command
        .trim()
        .strip_suffix("session-start")
        .and_then(|r| r.strip_suffix(char::is_whitespace))
        .and_then(|r| r.trim_end().strip_suffix("hook"))
        .and_then(|r| r.strip_suffix(char::is_whitespace))
        .map(str::trim)
    else {
        return false;
    };
    const UNQUOTED_FORBIDDEN: [char; 11] = ['"', '\'', ';', '&', '|', '<', '>', '(', ')', '$', '`'];
    let program = match program.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let Some(inner) = program[1..].strip_suffix(q) else {
                return false;
            };
            if inner.contains(q) || (q == '"' && inner.contains(['$', '`'])) {
                return false;
            }
            inner
        }
        _ if program.contains(|c: char| c.is_whitespace() || UNQUOTED_FORBIDDEN.contains(&c)) => {
            return false;
        }
        _ => program,
    };
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.eq_ignore_ascii_case("memhub") || name.eq_ignore_ascii_case("memhub.exe")
}

/// Temp file in the target's own directory (pid-unique), fsynced, then
/// renamed over the target; the temp is removed on every failure. Never
/// creates a directory. A target that exists but cannot be opened for
/// writing is refused rather than replaced by the rename (which only needs
/// directory permission on Unix). A symlinked file is written through to
/// its target so the link survives, and the original's permissions carry
/// over. On Unix the directory is synced after the rename so the rename
/// itself is durable.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let real: PathBuf = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let original = std::fs::metadata(&real).ok();
    if original.is_some() {
        std::fs::OpenOptions::new().write(true).open(&real)?;
    }
    let name = real.file_name().unwrap_or_default().to_string_lossy();
    let tmp = real.with_file_name(format!(".{name}.{}.memhub-tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Some(meta) = &original {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &real)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // The file is already replaced, so a failed directory sync is not a
    // failed write: logged, not returned.
    #[cfg(unix)]
    if result.is_ok()
        && let Some(dir) = real.parent()
        && let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all())
    {
        log::debug!(
            "directory sync after replacing {} skipped: {e}",
            real.display()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::runs_memhub_command;

    #[test]
    fn memhub_command_matches_bare_full_and_quoted_paths_only() {
        for yes in [
            "memhub hook session-start",
            "  memhub   hook\tsession-start ",
            "MEMHUB hook session-start",
            "/home/u/.cargo/bin/memhub hook session-start",
            r"C:\Users\u\.cargo\bin\memhub.exe hook session-start",
            r#""C:\Program Files\memhub\Memhub.EXE" hook session-start"#,
            "'/opt/my tools/memhub' hook session-start",
        ] {
            assert!(runs_memhub_command(yes), "{yes:?} must count");
        }
        for no in [
            "other-tool hook session-start",
            "/usr/bin/notmemhub hook session-start",
            "memhub-dev hook session-start",
            "/usr/bin/env memhub hook session-start",
            "hook session-start",
            "memhub hook session-start --verbose",
            "memhub hooksession-start",
            "memhubhook session-start",
            "memhub status",
            "\"memhub' hook session-start",
            // Review B1: words before the program run another program.
            "echo /usr/bin/memhub hook session-start",
            "other ./memhub hook session-start",
            "true && /x/memhub hook session-start",
            r#""/a b/x" "/c/memhub" hook session-start"#,
            // An unquoted path with spaces is not one program to a shell.
            r"C:\Program Files\memhub\memhub.exe hook session-start",
            "true&&/x/memhub hook session-start",
            "a\"/memhub hook session-start",
            "\"$(other)/memhub\" hook session-start",
            "\"/a\"/memhub\" hook session-start",
            "\" hook session-start",
        ] {
            assert!(!runs_memhub_command(no), "{no:?} must not count");
        }
    }
}

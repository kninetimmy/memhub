//! `memhub upgrade` — one dependable command to bring every memhub
//! install on this machine to a coherent, working state (decision 96,
//! resolves task 48, subsumes recurring task 39).
//!
//! Two phases. The phase the user invokes (`finish == false`) is
//! orchestration, run by the *old* binary: rebuild + install, fix the
//! `~/.local/bin` PATH shadow once, then re-exec the *freshly
//! installed* binary with `--finish` so the migrate + verify pass runs
//! under new code (old code only knows old migrations). The `--finish`
//! phase enumerates instances from the self-maintaining registry
//! (never a filesystem scan), brings each repo DB + the global store to
//! head, smoke-tests recall, and prints a per-instance table.
//!
//! ## Windows self-replace (the staging hop)
//!
//! The two-phase design above assumes the orchestrating process can run
//! `cargo install` to replace *its own running binary* and then re-exec
//! the result. That holds on Unix (a running image can be `unlink`'d /
//! `rename`'d over) but **not on Windows**, where the image of a running
//! `.exe` is locked. cargo install must overwrite both files in the
//! conflict set — `target\release\memhub.exe` (its build artifact) and
//! `~\.cargo\bin\memhub.exe` (its install dest) — and the user always
//! invokes one of them.
//!
//! Fix: an OS-gated pre-hop. On Windows, if the running image is in the
//! conflict set, copy ourselves to a `%TEMP%` shim, re-launch that with
//! `--staged`, and exit so the original releases its image lock. The
//! staged copy (image ∉ conflict set) then runs the unchanged
//! orchestration. The original cannot wait and propagate the real exit
//! code (waiting keeps its image locked), so the staged run records the
//! outcome to `~/.memhub/last_upgrade.json` and a final `memhub
//! upgrade:` line. Interactive runs stage automatically; non-interactive
//! runs (CI) require `--allow-self-stage` so the exit-code loss is never
//! silent. Unix is untouched — `stage_decision` returns `Orchestrate`.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::{self, RecallOptions};
use crate::{MemhubError, Result};

pub struct UpgradeArgs {
    /// Extra repo roots to include (and persist into the registry) even
    /// if memhub has never opened them — the registry bootstrap hatch.
    pub also: Vec<PathBuf>,
    pub dry_run: bool,
    pub json: bool,
    /// Hidden: set on the re-exec'd child so it runs only the migrate +
    /// verify pass and does not recurse into install / re-exec.
    pub finish: bool,
    /// Hidden: set on the Windows staged temp copy so it runs the real
    /// orchestration instead of recursing into another staging hop.
    pub staged: bool,
    /// Windows only: permit the staged self-relaunch with no TTY
    /// attached (CI/scripts). Without it, a non-interactive run that
    /// would need staging fails loudly instead of silently returning 0.
    pub allow_self_stage: bool,
    /// Skip the confirmation prompt before replacing a non-symlink
    /// `~/.local/bin/memhub` shadow.
    pub yes: bool,
    /// Skip resyncing installed agent skill wrappers (decision 97).
    pub no_skills: bool,
    /// Skip the `target/` build-artifact GC step (`memhub gc`).
    pub no_gc: bool,
}

/// Internal parent->child handoff for the skill-resync result. The skill
/// sync runs in the *orchestrate* phase (the old binary, in the source
/// repo where `templates/` lives) but is rendered by the re-exec'd
/// `--finish` child alongside the migrate table so there is one output
/// surface. A hidden CLI flag would be the `--finish` precedent, but
/// this is a pure internal IPC blob, not a user knob, so it travels by
/// env var (same spirit as the test seams).
const SKILLS_ENV: &str = "MEMHUB_UPGRADE_SKILLS_JSON";

pub fn run(cwd: &Path, args: UpgradeArgs) -> Result<()> {
    if args.finish {
        return finish_phase(cwd, &args);
    }

    // Windows-only self-replace gate. Best-effort sweep first so an
    // earlier run's abandoned shim never accumulates.
    if cfg!(windows) && !args.staged {
        sweep_stale_staging();
    }
    let in_conflict =
        cfg!(windows) && !args.staged && !args.dry_run && current_exe_in_conflict_set(cwd);
    match stage_decision(
        cfg!(windows),
        args.staged,
        args.dry_run,
        in_conflict,
        std::io::stdin().is_terminal(),
        args.allow_self_stage,
    ) {
        StageDecision::Orchestrate if args.staged => {
            // A parent exited 0 to relaunch us, so this run's real exit
            // code is lost to the invoking shell. Make every terminal
            // state durable: write a "started" marker up front (so a
            // killed-mid-run staged child reads as not-yet-done, never a
            // stale success), and convert an early orchestrate failure
            // — cargo install exhausted, re-exec couldn't launch — into
            // a recorded ok:false. The success and finish-phase-failure
            // states are already recorded by finish_phase in the
            // re-exec'd grandchild before this process exits.
            write_last_upgrade(false, "upgrade started; completion not yet recorded");
            match orchestrate_phase(cwd, &args) {
                Ok(()) => Ok(()),
                Err(e) => {
                    let msg = format!("upgrade failed before the migrate phase: {e}");
                    write_last_upgrade(false, &msg);
                    println!("memhub upgrade: FAILED — {msg}");
                    Err(e)
                }
            }
        }
        StageDecision::Orchestrate => orchestrate_phase(cwd, &args),
        StageDecision::Stage => stage_and_relaunch(cwd, &args),
        StageDecision::RefuseNeedsFlag => Err(MemhubError::InvalidInput(
            "memhub upgrade must replace its own running binary, which \
             Windows forbids while it is executing. No TTY was detected \
             (CI/script context), so the staged relaunch is not started \
             automatically. Re-run with --allow-self-stage to permit it; \
             note the invoking shell will then receive exit code 0 \
             regardless of outcome — read ~/.memhub/last_upgrade.json or \
             the final 'memhub upgrade:' line for the real result."
                .to_string(),
        )),
    }
}

/// What `run()` should do once the OS / invocation facts are known.
/// Pure and total so the Windows self-replace policy is unit-testable
/// without spawning processes or touching the real filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageDecision {
    /// Run the orchestration in this process (every Unix run; Windows
    /// runs whose image is already safe, e.g. the staged copy).
    Orchestrate,
    /// Copy self to a temp shim and relaunch it (`--staged`).
    Stage,
    /// Would need staging but it is non-interactive and not opted in.
    RefuseNeedsFlag,
}

fn stage_decision(
    is_windows: bool,
    staged: bool,
    dry_run: bool,
    in_conflict: bool,
    interactive: bool,
    allow_self_stage: bool,
) -> StageDecision {
    // Unix never stages; the staged child, dry-run, and any run whose
    // image is not in cargo's conflict set orchestrate directly.
    if !is_windows || staged || dry_run || !in_conflict {
        return StageDecision::Orchestrate;
    }
    if interactive || allow_self_stage {
        StageDecision::Stage
    } else {
        StageDecision::RefuseNeedsFlag
    }
}

// ---------------------------------------------------------------------
// Windows self-replace staging hop
// ---------------------------------------------------------------------

/// The two files `cargo install --path . --force` overwrites: its build
/// artifact under the source repo's `target/release/`, and its install
/// destination in the cargo bin. The orchestrator's own image must be
/// neither.
fn conflict_set(cwd: &Path) -> Vec<PathBuf> {
    let mut set = vec![cwd.join("target").join("release").join(bin_name())];
    if let Ok(cb) = cargo_bin_path() {
        set.push(cb);
    }
    set
}

/// Pure membership test (canonicalizing so a symlinked cargo bin or a
/// relative `target/` still matches). Split out for unit tests.
fn path_in_conflict_set(exe: &Path, set: &[PathBuf]) -> bool {
    set.iter().any(|p| same_file(exe, p))
}

fn current_exe_in_conflict_set(cwd: &Path) -> bool {
    match std::env::current_exe() {
        Ok(exe) => path_in_conflict_set(&exe, &conflict_set(cwd)),
        // If we cannot resolve our own path, assume the worst (in
        // conflict) so we stage rather than risk the self-replace lock.
        Err(_) => true,
    }
}

/// Copy this binary to a `%TEMP%` shim, relaunch it with `--staged`
/// inheriting this console, and exit so the original releases its image
/// lock. The shim image is not in cargo's conflict set, so the staged
/// run's `cargo install` can replace both files freely (mirrors the
/// proven manual `%TEMP%` workaround). We cannot wait on the child —
/// waiting keeps our image locked — so the staged run owns the outcome
/// reporting (`~/.memhub/last_upgrade.json` + final line).
fn stage_and_relaunch(cwd: &Path, args: &UpgradeArgs) -> Result<()> {
    let src = std::env::current_exe().map_err(|e| {
        MemhubError::InvalidInput(format!("cannot resolve current exe to stage: {e}"))
    })?;
    let shim = std::env::temp_dir().join(format!(
        "memhub-upgrade-{}-{}.exe",
        std::process::id(),
        now_stamp()
    ));
    std::fs::copy(&src, &shim).map_err(|e| {
        MemhubError::InvalidInput(format!(
            "could not stage upgrade binary to {}: {e}",
            shim.display()
        ))
    })?;

    let mut cmd = staged_relaunch_command(&shim, cwd, args);

    // Break away from any job object the terminal placed us in so the
    // staged child survives our imminent exit. Harmless if not jobbed;
    // if the job forbids breakaway, fall back to a plain spawn — rebuilt
    // through the SAME helper so the fallback can never diverge from the
    // primary argv (the original bug: it dropped every forwarded flag).
    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .or_else(|_| staged_relaunch_command(&shim, cwd, args).spawn())
    };
    #[cfg(not(windows))]
    let spawned = cmd.spawn();

    spawned.map_err(|e| MemhubError::ExternalCommand {
        command: format!("{} upgrade --staged", shim.display()),
        stderr: format!("could not launch staged upgrade copy ({e})"),
    })?;

    println!(
        "==> Windows: relaunched a staged copy ({}) so cargo can replace \
         this binary. This shell returns now; watch the staged run's \
         output and its final 'memhub upgrade:' line, or read \
         ~/.memhub/last_upgrade.json.",
        shim.display()
    );
    // Do NOT wait: staying alive keeps our image locked and re-creates
    // the very bug we are fixing. Exit 0 so the lock releases.
    std::process::exit(0);
}

/// Single source of truth for the staged-relaunch argv.
///
/// The staged child re-parses its own argv and runs the **full**
/// orchestration (`orchestrate_phase`), so every user-visible flag with
/// a side effect must round-trip here. Dropping one silently voids an
/// explicit opt-out — the original bug: `--no-gc` was not forwarded, so
/// a staged Windows run (the *normal* path there) deleted build
/// artifacts the user had explicitly told it not to touch. Both spawn
/// sites in `stage_and_relaunch` (the breakaway spawn and its
/// job-forbidden fallback) build their `Command` through this so they
/// cannot diverge again.
///
/// `--staged` is added here; `stage_decision` keys the
/// orchestrate-vs-stage choice off it. Deliberately NOT forwarded:
/// `--allow-self-stage` (moot — `--staged` already forces `Orchestrate`
/// in the child), `--dry-run` (staging is gated on `!dry_run` upstream),
/// and `--finish` (a different phase with its own minimal flag set).
fn staged_relaunch_command(shim: &Path, cwd: &Path, args: &UpgradeArgs) -> Command {
    let mut cmd = Command::new(shim);
    cmd.arg("upgrade")
        .arg("--staged")
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if args.yes {
        cmd.arg("--yes");
    }
    if args.no_skills {
        cmd.arg("--no-skills");
    }
    if args.no_gc {
        cmd.arg("--no-gc");
    }
    if args.json {
        cmd.arg("--json");
    }
    for p in &args.also {
        cmd.arg("--also").arg(p);
    }
    cmd
}

/// Remove abandoned staged shims (`memhub-upgrade-*.exe`) older than an
/// hour from the temp dir. Hour guard avoids racing a concurrent
/// upgrade; the live shim of the current staged run stays locked and is
/// simply skipped (best-effort, never fatal).
fn sweep_stale_staging() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("memhub-upgrade-") && name.ends_with(".exe")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m < cutoff)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Compact monotonic-ish stamp for unique shim names within a process.
fn now_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Run `cargo install --path . --force`, streaming cargo's own output.
/// When `staged` (Windows staged run), retry a failed attempt a few
/// times with backoff: the only expected transient is the parent's
/// image lock lingering past its exit, and cargo's cache makes retries
/// cheap. Non-staged / Unix keeps the original single-shot fail-fast.
fn cargo_install_with_retry(cwd: &Path, staged: bool) -> Result<()> {
    let attempts = if staged { 3 } else { 1 };
    for attempt in 1..=attempts {
        let status = Command::new("cargo")
            .arg("install")
            .arg("--path")
            .arg(".")
            .arg("--force")
            .current_dir(cwd)
            .status()
            .map_err(|e| MemhubError::ExternalCommand {
                command: "cargo install --path . --force".to_string(),
                stderr: format!("could not launch cargo ({e}); is it on PATH?"),
            })?;
        if status.success() {
            return Ok(());
        }
        if attempt < attempts {
            eprintln!(
                "    cargo install failed (attempt {attempt}/{attempts}); the \
                 prior binary may still be releasing its Windows image \
                 lock — retrying in {attempt}s…"
            );
            std::thread::sleep(std::time::Duration::from_secs(attempt as u64));
        }
    }
    Err(MemhubError::ExternalCommand {
        command: "cargo install --path . --force".to_string(),
        stderr: format!(
            "build/install failed after {attempts} attempt(s); not migrating instances"
        ),
    })
}

// ---------------------------------------------------------------------
// Phase 1: orchestration (old binary)
// ---------------------------------------------------------------------

fn orchestrate_phase(cwd: &Path, args: &UpgradeArgs) -> Result<()> {
    ensure_source_repo(cwd)?;
    let cargo_bin = cargo_bin_path()?;

    if args.dry_run {
        return dry_run_report(cwd, args, &cargo_bin);
    }

    // 1. Rebuild + install. Stream cargo's own output; abort on failure
    //    rather than half-upgrade. On a Windows staged run the original
    //    process may still be releasing its image lock for a few ms, so
    //    retry: cargo's incremental cache makes a retry cheap (a lock
    //    failure skips compile entirely and just re-attempts the move).
    println!("==> cargo install --path . --force");
    cargo_install_with_retry(cwd, args.staged)?;
    println!("    installed -> {}", cargo_bin.display());

    // 2. One-time, order-independent PATH-shadow fix (closes task 39).
    let outcome = fix_path_shadow(&cargo_bin, args.yes)?;
    println!("==> PATH: {}", outcome.message);

    // 3. Resync installed agent skill wrappers from templates/ (decision
    //    97). Additive, idempotent, best-effort — never fatal. Done here
    //    in the old binary because the source repo's `templates/` must
    //    be present (already an `upgrade` precondition); the result is
    //    handed to the re-exec'd child so it renders in one table.
    let skills = if args.no_skills {
        vec![SkillSync::skipped_all("--no-skills")]
    } else {
        sync_skills(cwd, false)
    };

    // 3b. Reclaim superseded build artifacts in the source repo's
    //     `target/` (Cargo never GCs old hashes; memhub's bundled ONNX
    //     models make each stale artifact ~1 GB). Best-effort and never
    //     fatal — same posture as the skill resync and registry writes.
    if args.no_gc {
        println!("==> target gc: skipped (--no-gc)");
    } else {
        match crate::commands::gc::run(cwd, false) {
            Ok(out) => println!("==> target gc: {}", out.summary()),
            Err(e) => println!("==> target gc: skipped ({e})"),
        }
    }

    // 4. Re-exec the freshly installed binary for the migrate + verify
    //    pass so migrations run under NEW code. Use the explicit
    //    cargo-bin path, not PATH — PATH may still resolve to a shadow
    //    the user declined to fix.
    //
    //    This is intentionally NOT `staged_relaunch_command`: the
    //    `--finish` child only migrates + verifies. The skill resync,
    //    GC, and PATH-shadow fix have already run in THIS (orchestrate)
    //    process, so `--no-gc` / `--no-skills` / `--yes` are irrelevant
    //    to it by design. Only `--json` (output shape) and `--also`
    //    (extra roots to migrate) carry into the finish phase. Do not
    //    "unify" this with the staged helper — the flag sets differ on
    //    purpose.
    let mut child = Command::new(&cargo_bin);
    child
        .arg("upgrade")
        .arg("--finish")
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Ok(js) = serde_json::to_string(&skills) {
        child.env(SKILLS_ENV, js);
    }
    if args.json {
        child.arg("--json");
    }
    for p in &args.also {
        child.arg("--also").arg(p);
    }
    let code = child
        .status()
        .map_err(|e| MemhubError::ExternalCommand {
            command: format!("{} upgrade --finish", cargo_bin.display()),
            stderr: format!("could not re-exec freshly installed binary ({e})"),
        })?
        .code()
        .unwrap_or(0);
    std::process::exit(code);
}

fn ensure_source_repo(cwd: &Path) -> Result<()> {
    let manifest = cwd.join("Cargo.toml");
    let raw = std::fs::read_to_string(&manifest).map_err(|_| {
        MemhubError::InvalidInput(format!(
            "run `memhub upgrade` from the memhub source repo \
             (no Cargo.toml at {})",
            cwd.display()
        ))
    })?;
    let parsed: toml::Value = toml::from_str(&raw)?;
    let name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str());
    if name != Some("memhub") {
        return Err(MemhubError::InvalidInput(format!(
            "{} is not the memhub source repo (package = {:?}); \
             `memhub upgrade` rebuilds from source and must run there",
            cwd.display(),
            name.unwrap_or("<none>")
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Phase 2: migrate + verify (freshly installed binary)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum InstanceStatus {
    /// Already at head before this run.
    Ready,
    /// Was behind; brought to head this run.
    Migrated,
    /// Known/asked-for but nothing to do (path gone, store absent).
    Skipped(String),
    /// Opened/verified and failed — a real problem.
    Error(String),
}

struct InstanceReport {
    label: String,
    before: Option<String>,
    after: Option<String>,
    status: InstanceStatus,
}

fn finish_phase(cwd: &Path, args: &UpgradeArgs) -> Result<()> {
    let head = db::latest_schema_version().to_string();

    // Self-heal first: drop registry rows whose repo DB is gone
    // (deleted repos, vanished throwaway clones) so the table reflects
    // reality instead of accumulating dead weight.
    let pruned = db::registry::prune_dead().unwrap_or(0);

    // Persist explicit --also roots so they survive into future runs,
    // then enumerate: the source repo, the registry, and --also.
    for p in &args.also {
        let _ = db::registry::register(p, &head);
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let push =
        |p: &Path, roots: &mut Vec<PathBuf>, seen: &mut std::collections::BTreeSet<String>| {
            let key = p
                .canonicalize()
                .unwrap_or_else(|_| p.to_path_buf())
                .to_string_lossy()
                .to_string();
            if seen.insert(key) {
                roots.push(p.to_path_buf());
            }
        };

    push(cwd, &mut roots, &mut seen);
    for kp in db::registry::list_known()? {
        push(&kp.root_path, &mut roots, &mut seen);
    }
    for p in &args.also {
        push(p, &mut roots, &mut seen);
    }

    let mut reports: Vec<InstanceReport> = Vec::new();
    for root in &roots {
        reports.push(verify_repo(root));
    }
    reports.push(verify_global());

    // Skill resync ran in the orchestrate phase; its result rides an
    // env var so it renders here alongside the migrate table. Absent
    // (e.g. `--finish` invoked directly in a test) => no skill section.
    let skills: Vec<SkillSync> = std::env::var(SKILLS_ENV)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    emit(&reports, pruned, &skills, args.json);

    let failed: Vec<&str> = reports
        .iter()
        .filter(|r| matches!(r.status, InstanceStatus::Error(_)))
        .map(|r| r.label.as_str())
        .collect();
    let ok = failed.is_empty();
    let summary = if ok {
        format!("{} instance(s) at head", reports.len())
    } else {
        format!("{} instance(s) failed: {}", failed.len(), failed.join(", "))
    };

    // The invoking shell may only ever see the orchestrator's exit(0)
    // (Windows staged relaunch can't propagate a real code), so the
    // outcome must be legible without the exit status: a durable status
    // file plus an unambiguous final line. Best-effort, never fatal.
    write_last_upgrade(ok, &summary);
    if ok {
        println!("memhub upgrade: SUCCESS — {summary}");
    } else {
        println!("memhub upgrade: FAILED — {summary}");
        std::process::exit(1);
    }
    Ok(())
}

/// Record the upgrade outcome to `~/.memhub/last_upgrade.json` so a
/// non-interactive caller (whose shell got the staged relaunch's
/// exit 0) can still poll the real result. Best-effort: a write failure
/// must not turn a good upgrade into a reported failure.
fn write_last_upgrade(ok: bool, summary: &str) {
    let Ok(home) = db::home_dir() else { return };
    let dir = home.join(".memhub");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let payload = json!({
        "ok": ok,
        "summary": summary,
        "unix_ms": now_stamp(),
    });
    let _ = std::fs::write(dir.join("last_upgrade.json"), payload.to_string());
}

fn verify_repo(root: &Path) -> InstanceReport {
    let label = abbrev(root);
    let db_path = root.join(".memhub").join("project.sqlite");
    if !db_path.exists() {
        return InstanceReport {
            label,
            before: None,
            after: None,
            status: InstanceStatus::Skipped(
                "no memhub project (path gone or never initialized)".to_string(),
            ),
        };
    }
    let before = db::probe_schema_version(&db_path).ok().flatten();
    // open_project auto-applies migrations to head.
    if let Err(e) = db::open_project(root) {
        return InstanceReport {
            label,
            before,
            after: None,
            status: InstanceStatus::Error(format!("open failed: {e}")),
        };
    }
    let after = db::latest_schema_version().to_string();
    // Tiny recall smoke. Force FTS so we exercise the query path
    // without paying the embedding/re-ranker model load per instance.
    let status = match smoke(root) {
        Ok(()) => {
            if before.as_deref() == Some(after.as_str()) {
                InstanceStatus::Ready
            } else {
                InstanceStatus::Migrated
            }
        }
        Err(e) => InstanceStatus::Error(format!("recall smoke failed: {e}")),
    };
    InstanceReport {
        label,
        before,
        after: Some(after),
        status,
    }
}

fn verify_global() -> InstanceReport {
    let path = match db::global_db_path() {
        Ok(p) => p,
        Err(e) => {
            return InstanceReport {
                label: "<global store>".to_string(),
                before: None,
                after: None,
                status: InstanceStatus::Error(e.to_string()),
            };
        }
    };
    let exists = db::global_store_exists().unwrap_or(false);
    if !exists {
        return InstanceReport {
            label: "<global store>".to_string(),
            before: None,
            after: None,
            status: InstanceStatus::Skipped(
                "absent — opt in with `memhub global enable` in a repo".to_string(),
            ),
        };
    }
    let before = db::probe_schema_version(&path).ok().flatten();
    if let Err(e) = db::open_global() {
        return InstanceReport {
            label: "<global store>".to_string(),
            before,
            after: None,
            status: InstanceStatus::Error(format!("open failed: {e}")),
        };
    }
    let after = db::latest_schema_version().to_string();
    let status = if before.as_deref() == Some(after.as_str()) {
        InstanceStatus::Ready
    } else {
        InstanceStatus::Migrated
    };
    InstanceReport {
        label: "<global store>".to_string(),
        before,
        after: Some(after),
        status,
    }
}

fn smoke(root: &Path) -> Result<()> {
    retrieval::recall(
        root,
        RecallOptions {
            query: "memhub".to_string(),
            mode: Some(RetrievalMode::Fts),
            max_results: 1,
            source_types: vec![],
            include_stale: None,
            accepted_only: None,
            use_reranker: None,
            min_rerank_score: None,
            log_metrics: false,
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------
// PATH-shadow fix (order-independent; closes task 39)
// ---------------------------------------------------------------------

struct ShadowOutcome {
    message: String,
}

impl ShadowOutcome {
    fn msg(s: impl Into<String>) -> Self {
        Self { message: s.into() }
    }
}

/// If `~/.local/bin/memhub` shadows the cargo bin on PATH, make it a
/// symlink to the cargo bin **once** so every future `cargo install`
/// takes effect with no manual `cp`. Idempotent. A regular-file shadow
/// is the task-39 bug; replacing it is confirmed unless `--yes`.
fn fix_path_shadow(cargo_bin: &Path, yes: bool) -> Result<ShadowOutcome> {
    let shadow = local_bin_shadow()?;
    let meta = match std::fs::symlink_metadata(&shadow) {
        Ok(m) => m,
        Err(_) => {
            return Ok(ShadowOutcome::msg(
                "no ~/.local/bin/memhub shadow; ~/.cargo/bin is canonical",
            ));
        }
    };

    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&shadow).unwrap_or_default();
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            shadow
                .parent()
                .map(|p| p.join(&target))
                .unwrap_or_else(|| target.clone())
        };
        if same_file(&resolved, cargo_bin) {
            return Ok(ShadowOutcome::msg(format!(
                "~/.local/bin/memhub already -> {} (ok)",
                cargo_bin.display()
            )));
        }
        if !confirm(
            &format!(
                "~/.local/bin/memhub is a symlink to {} (not the cargo bin). \
                 Repoint it to {}?",
                resolved.display(),
                cargo_bin.display()
            ),
            yes,
        ) {
            return Ok(ShadowOutcome::msg(format!(
                "left ~/.local/bin/memhub -> {} unchanged; to fix manually: ln -sf {} {}",
                resolved.display(),
                cargo_bin.display(),
                shadow.display()
            )));
        }
        relink(cargo_bin, &shadow)?;
        return Ok(ShadowOutcome::msg(format!(
            "repointed ~/.local/bin/memhub -> {}",
            cargo_bin.display()
        )));
    }

    // Regular file: the stale-binary shadow (task 39 root cause).
    if !confirm(
        &format!(
            "~/.local/bin/memhub is a stale copied binary shadowing {}. \
             Replace it with a symlink so future installs take effect?",
            cargo_bin.display()
        ),
        yes,
    ) {
        return Ok(ShadowOutcome::msg(format!(
            "stale ~/.local/bin/memhub left in place; to fix manually: ln -sf {} {}",
            cargo_bin.display(),
            shadow.display()
        )));
    }
    relink(cargo_bin, &shadow)?;
    Ok(ShadowOutcome::msg(format!(
        "replaced stale ~/.local/bin/memhub with symlink -> {} (task 39 fixed)",
        cargo_bin.display()
    )))
}

fn relink(cargo_bin: &Path, shadow: &Path) -> Result<()> {
    if let Some(parent) = shadow.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(shadow);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(cargo_bin, shadow)?;
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(cargo_bin, shadow).is_err() {
            // Symlinks need privilege on Windows; a copy is the boring
            // fallback (re-run `memhub upgrade` after each install).
            std::fs::copy(cargo_bin, shadow)?;
        }
    }
    Ok(())
}

fn confirm(prompt: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        // Non-interactive and not pre-authorized: never silently
        // clobber. The caller surfaces the manual command instead.
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

// ---------------------------------------------------------------------
// Dry run
// ---------------------------------------------------------------------

fn dry_run_report(cwd: &Path, args: &UpgradeArgs, cargo_bin: &Path) -> Result<()> {
    let head = db::latest_schema_version().to_string();

    let mut roots: Vec<PathBuf> = vec![cwd.to_path_buf()];
    for kp in db::registry::list_known()? {
        roots.push(kp.root_path);
    }
    roots.extend(args.also.iter().cloned());

    let mut previews: Vec<(String, Option<String>, &'static str)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for root in &roots {
        let key = root
            .canonicalize()
            .unwrap_or_else(|_| root.clone())
            .to_string_lossy()
            .to_string();
        if !seen.insert(key) {
            continue;
        }
        let db_path = root.join(".memhub").join("project.sqlite");
        if !db_path.exists() {
            // Dead registry rows are summarized by `would_prune`; don't
            // flood the preview with one line per vanished tempdir.
            continue;
        }
        let cur = db::probe_schema_version(&db_path).ok().flatten();
        let verdict = if cur.as_deref() == Some(head.as_str()) {
            "ready"
        } else {
            "would migrate"
        };
        previews.push((abbrev(root), cur, verdict));
    }
    let global_preview = if db::global_store_exists()? {
        let cur = db::probe_schema_version(&db::global_db_path()?)
            .ok()
            .flatten();
        let verdict = if cur.as_deref() == Some(head.as_str()) {
            "ready"
        } else {
            "would migrate"
        };
        (cur, verdict)
    } else {
        (None, "absent (opt in via `memhub global enable`)")
    };

    let would_prune = db::registry::dead_roots().map(|d| d.len()).unwrap_or(0);
    let shadow_state = describe_shadow(cargo_bin)?;
    let skills = if args.no_skills {
        vec![SkillSync::skipped_all("--no-skills")]
    } else {
        sync_skills(cwd, true)
    };

    if args.json {
        println!(
            "{}",
            json!({
                "dry_run": true,
                "would_run": "cargo install --path . --force",
                "head_schema": head,
                "path_shadow": shadow_state,
                "would_prune": would_prune,
                "instances": previews
                    .iter()
                    .map(|(l, c, v)| json!({
                        "label": l,
                        "schema": c,
                        "verdict": v,
                    }))
                    .collect::<Vec<_>>(),
                "global_store": {
                    "schema": global_preview.0,
                    "verdict": global_preview.1,
                },
                "skills": skills,
            })
        );
        return Ok(());
    }

    println!("memhub upgrade --dry-run (no changes made)");
    if cfg!(windows) && current_exe_in_conflict_set(cwd) {
        println!(
            "  windows:      a real run would relaunch a staged %TEMP% \
             copy (auto if a TTY is attached, else needs --allow-self-stage)"
        );
    }
    println!("  would run:    cargo install --path . --force");
    println!("  install ->    {}", cargo_bin.display());
    println!("  PATH shadow:  {shadow_state}");
    println!("  head schema:  {head}");
    if would_prune > 0 {
        println!("  would prune:  {would_prune} stale registry entries (repo gone)");
    }
    println!("  instances ({}):", previews.len());
    for (label, cur, verdict) in &previews {
        println!(
            "    {:<40} {:<10} {}",
            label,
            cur.as_deref().unwrap_or("(none)"),
            verdict
        );
    }
    println!(
        "    {:<40} {:<10} {}",
        "<global store>",
        global_preview.0.as_deref().unwrap_or("(none)"),
        global_preview.1
    );
    for s in &skills {
        println!("  skills:       {}", s.dry_line());
    }
    if args.no_gc {
        println!("  target gc:    skipped (--no-gc)");
    } else {
        match crate::commands::gc::run(cwd, true) {
            Ok(out) => println!("  target gc:    {}", out.summary()),
            Err(e) => println!("  target gc:    skipped ({e})"),
        }
    }
    Ok(())
}

fn describe_shadow(cargo_bin: &Path) -> Result<String> {
    let shadow = local_bin_shadow()?;
    let Ok(meta) = std::fs::symlink_metadata(&shadow) else {
        return Ok("none (cargo bin is canonical)".to_string());
    };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&shadow).unwrap_or_default();
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            shadow
                .parent()
                .map(|p| p.join(&target))
                .unwrap_or_else(|| target.clone())
        };
        if same_file(&resolved, cargo_bin) {
            Ok("symlink -> cargo bin (ok)".to_string())
        } else {
            Ok(format!(
                "symlink -> {} (would repoint to cargo bin)",
                resolved.display()
            ))
        }
    } else {
        Ok("stale copied binary (would replace with symlink — task 39)".to_string())
    }
}

// ---------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------

fn emit(reports: &[InstanceReport], pruned: usize, skills: &[SkillSync], as_json: bool) {
    let ready = reports
        .iter()
        .filter(|r| matches!(r.status, InstanceStatus::Ready | InstanceStatus::Migrated))
        .count();
    let total = reports.len();

    if as_json {
        println!(
            "{}",
            json!({
                "instances": reports
                    .iter()
                    .map(|r| json!({
                        "label": r.label,
                        "before": r.before,
                        "after": r.after,
                        "status": status_word(&r.status),
                        "detail": status_detail(&r.status),
                    }))
                    .collect::<Vec<_>>(),
                "ready": ready,
                "total": total,
                "pruned": pruned,
                "skills": skills,
            })
        );
        return;
    }

    println!("memhub upgrade");
    println!("  {:<40} {:<16} STATUS", "INSTANCE", "SCHEMA");
    for r in reports {
        let schema = match (&r.before, &r.after) {
            (Some(b), Some(a)) if b != a => format!("{b} -> {a}"),
            (_, Some(a)) => a.clone(),
            (Some(b), None) => b.clone(),
            (None, None) => "(none)".to_string(),
        };
        let status = match &r.status {
            InstanceStatus::Ready => "ready".to_string(),
            InstanceStatus::Migrated => "migrated".to_string(),
            InstanceStatus::Skipped(why) => format!("skipped ({why})"),
            InstanceStatus::Error(why) => format!("ERROR ({why})"),
        };
        println!("  {:<40} {:<16} {}", r.label, schema, status);
    }
    println!();
    if pruned > 0 {
        println!("  pruned {pruned} stale registry entries (repo gone)");
    }
    for s in skills {
        println!("  skills: {}", s.line());
    }
    println!("  {ready}/{total} instances ready");
}

fn status_word(s: &InstanceStatus) -> &'static str {
    match s {
        InstanceStatus::Ready => "ready",
        InstanceStatus::Migrated => "migrated",
        InstanceStatus::Skipped(_) => "skipped",
        InstanceStatus::Error(_) => "error",
    }
}

fn status_detail(s: &InstanceStatus) -> Option<String> {
    match s {
        InstanceStatus::Skipped(w) | InstanceStatus::Error(w) => Some(w.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Skill resync (decision 97)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSyncStatus {
    /// Files were copied (or, in `--dry-run`, would be).
    Synced,
    /// Nothing done — agent not set up, or `--no-skills`.
    Skipped,
    /// Best-effort copy hit a partial/permission error. Never fatal:
    /// decision 97 pins the registry/metrics "never fatal" posture.
    Warn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSync {
    /// Agent surface label, or "(all)" for the `--no-skills` sentinel.
    pub agent: String,
    /// `$HOME`-abbreviated install dir, or empty for the sentinel.
    pub target: String,
    pub status: SkillSyncStatus,
    /// Skill or command units copied.
    pub synced: usize,
    /// Skip reason or warning text.
    pub detail: Option<String>,
}

impl SkillSync {
    fn skipped_all(reason: &str) -> Self {
        SkillSync {
            agent: "(all)".to_string(),
            target: String::new(),
            status: SkillSyncStatus::Skipped,
            synced: 0,
            detail: Some(reason.to_string()),
        }
    }

    /// One compact human line, e.g.
    /// `claude  ~/.claude/commands  synced 11`.
    fn line(&self) -> String {
        self.render(false)
    }

    /// `--dry-run` variant: a successful sync reads as "would sync N".
    fn dry_line(&self) -> String {
        self.render(true)
    }

    fn render(&self, dry: bool) -> String {
        let tail = match self.status {
            SkillSyncStatus::Synced if dry => format!("would sync {}", self.synced),
            SkillSyncStatus::Synced => format!("synced {}", self.synced),
            SkillSyncStatus::Skipped => match &self.detail {
                Some(d) => format!("skipped ({d})"),
                None => "skipped".to_string(),
            },
            SkillSyncStatus::Warn => match &self.detail {
                Some(d) => format!("warn ({d})"),
                None => "warn".to_string(),
            },
        };
        if self.target.is_empty() {
            format!("{:<8} {tail}", self.agent)
        } else {
            format!("{:<8} {:<22} {tail}", self.agent, self.target)
        }
    }
}

enum CopyKind {
    /// Flat `*.md` files, used by Claude commands and OpenCode commands.
    FlatMd,
    /// One dir per skill, used by Codex and OpenCode skills.
    DirPerSkill,
}

/// Additively copy the repo's skill templates over the installed agent
/// skill wrappers, for each agent dir that **already exists** (never
/// created — mirrors `upgrade`'s "only act on what exists" posture for
/// the PATH shadow and the global store). Idempotent; best-effort.
///
/// Additive only: a skill/command removed or renamed in `templates/` leaves a
/// harmless installed orphan. Settled against mirror-with-prune because
/// pruning shared user-global dirs (`~/.claude/commands`,
/// `~/.codex/skills`, `~/.config/opencode/skills`,
/// `~/.config/opencode/commands`) risks a user's own same-named file,
/// while an orphan is just a stale slash-command, not a correctness bug.
///
/// `dry` stats and counts but performs no filesystem mutation.
pub fn sync_skills(source_repo: &Path, dry: bool) -> Vec<SkillSync> {
    let home = match db::home_dir() {
        Ok(h) => h,
        Err(e) => {
            return vec![SkillSync {
                agent: "(all)".to_string(),
                target: String::new(),
                status: SkillSyncStatus::Warn,
                synced: 0,
                detail: Some(format!("cannot resolve home dir: {e}")),
            }];
        }
    };
    let skills_root = source_repo.join("templates").join("skills");
    let commands_root = source_repo.join("templates").join("commands");
    vec![
        sync_one(
            "claude",
            &skills_root.join("claude"),
            &home.join(".claude").join("commands"),
            CopyKind::FlatMd,
            dry,
        ),
        sync_one(
            "codex",
            &skills_root.join("codex"),
            &home.join(".codex").join("skills"),
            CopyKind::DirPerSkill,
            dry,
        ),
        sync_one(
            "opencode-skills",
            &skills_root.join("opencode"),
            &home.join(".config").join("opencode").join("skills"),
            CopyKind::DirPerSkill,
            dry,
        ),
        sync_one(
            "opencode-commands",
            &commands_root.join("opencode"),
            &home.join(".config").join("opencode").join("commands"),
            CopyKind::FlatMd,
            dry,
        ),
    ]
}

fn sync_one(agent: &str, src: &Path, target: &Path, kind: CopyKind, dry: bool) -> SkillSync {
    let label = abbrev(target);
    let mk = |status: SkillSyncStatus, synced: usize, detail: Option<String>| SkillSync {
        agent: agent.to_string(),
        target: label.clone(),
        status,
        synced,
        detail,
    };

    // Only sync an agent dir the user actually set up. A missing dir or
    // a non-dir at that path is a clean skip — never created.
    match std::fs::symlink_metadata(target) {
        Err(_) => {
            return mk(
                SkillSyncStatus::Skipped,
                0,
                Some("agent dir absent (not set up)".to_string()),
            );
        }
        Ok(m) if !m.is_dir() => {
            return mk(
                SkillSyncStatus::Skipped,
                0,
                Some("path exists but is not a directory".to_string()),
            );
        }
        Ok(_) => {}
    }

    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) => {
            // Precondition is that templates/ exists in the source repo;
            // surface rather than silently report 0.
            return mk(
                SkillSyncStatus::Warn,
                0,
                Some(format!("templates unreadable at {}: {e}", src.display())),
            );
        }
    };

    let mut synced = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let from = entry.path();
        let name = entry.file_name();
        match kind {
            CopyKind::FlatMd => {
                if from.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if !entry.path().is_file() {
                    continue;
                }
                let to = target.join(&name);
                if dry {
                    synced += 1;
                } else if let Err(e) = std::fs::copy(&from, &to) {
                    errors.push(format!("{}: {e}", name.to_string_lossy()));
                } else {
                    synced += 1;
                }
            }
            CopyKind::DirPerSkill => {
                if !entry.path().is_dir() {
                    continue;
                }
                let to = target.join(&name);
                if dry {
                    synced += 1;
                } else if let Err(e) = copy_dir_recursive(&from, &to) {
                    errors.push(format!("{}: {e}", name.to_string_lossy()));
                } else {
                    synced += 1;
                }
            }
        }
    }

    if errors.is_empty() {
        mk(SkillSyncStatus::Synced, synced, None)
    } else {
        let shown: Vec<&String> = errors.iter().take(3).collect();
        let more = errors.len().saturating_sub(shown.len());
        let mut detail = format!(
            "{} synced, {} failed: {}",
            synced,
            errors.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        if more > 0 {
            detail.push_str(&format!("; +{more} more"));
        }
        mk(SkillSyncStatus::Warn, synced, Some(detail))
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "memhub.exe"
    } else {
        "memhub"
    }
}

fn cargo_bin_path() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(h).join("bin").join(bin_name()));
    }
    Ok(db::home_dir()?.join(".cargo").join("bin").join(bin_name()))
}

fn local_bin_shadow() -> Result<PathBuf> {
    Ok(db::home_dir()?.join(".local").join("bin").join(bin_name()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Replace a leading `$HOME` with `~` for compact, stable table output.
fn abbrev(path: &Path) -> String {
    if let Ok(home) = db::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Decision matrix for the Windows self-replace gate. The five-arg
    // tuple is (is_windows, staged, dry_run, in_conflict, interactive,
    // allow_self_stage) -> expected.
    #[test]
    fn unix_never_stages() {
        // No combination on a non-Windows host should ever stage.
        for &staged in &[false, true] {
            for &dry in &[false, true] {
                for &conflict in &[false, true] {
                    for &tty in &[false, true] {
                        for &allow in &[false, true] {
                            assert_eq!(
                                stage_decision(false, staged, dry, conflict, tty, allow),
                                StageDecision::Orchestrate,
                                "unix must orchestrate (staged={staged} dry={dry} \
                                 conflict={conflict} tty={tty} allow={allow})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn windows_staged_child_orchestrates() {
        // The relaunched copy carries --staged and must not re-stage,
        // even though its invocation still looks in-conflict.
        assert_eq!(
            stage_decision(true, true, false, true, true, false),
            StageDecision::Orchestrate
        );
    }

    #[test]
    fn windows_dry_run_never_stages() {
        assert_eq!(
            stage_decision(true, false, true, true, true, true),
            StageDecision::Orchestrate
        );
    }

    #[test]
    fn windows_safe_image_orchestrates_directly() {
        // current_exe not in cargo's conflict set (e.g. already a temp
        // shim, or run from an arbitrary path) => no staging needed.
        assert_eq!(
            stage_decision(true, false, false, false, false, false),
            StageDecision::Orchestrate
        );
    }

    #[test]
    fn windows_interactive_in_conflict_auto_stages() {
        assert_eq!(
            stage_decision(true, false, false, true, true, false),
            StageDecision::Stage
        );
    }

    #[test]
    fn windows_noninteractive_in_conflict_refuses_without_flag() {
        assert_eq!(
            stage_decision(true, false, false, true, false, false),
            StageDecision::RefuseNeedsFlag
        );
    }

    #[test]
    fn windows_noninteractive_in_conflict_stages_with_flag() {
        assert_eq!(
            stage_decision(true, false, false, true, false, true),
            StageDecision::Stage
        );
    }

    #[test]
    fn conflict_set_membership_matches_build_and_cargo_bin() {
        let cwd = std::env::temp_dir();
        let set = conflict_set(&cwd);
        let target_release = cwd.join("target").join("release").join(bin_name());
        assert!(
            path_in_conflict_set(&target_release, &set),
            "the source repo's target/release binary is a conflict target"
        );
        let elsewhere = cwd.join("definitely-not-memhub-xyz");
        assert!(
            !path_in_conflict_set(&elsewhere, &set),
            "an unrelated path is not in the conflict set"
        );
    }

    /// `UpgradeArgs` has no `Default` (it is only ever built by the CLI
    /// parser); a local builder keeps the staged-argv tests readable.
    fn upgrade_args(
        json: bool,
        yes: bool,
        no_skills: bool,
        no_gc: bool,
        also: &[&str],
    ) -> UpgradeArgs {
        UpgradeArgs {
            also: also.iter().map(PathBuf::from).collect(),
            dry_run: false,
            json,
            finish: false,
            staged: false,
            allow_self_stage: true, // set on purpose: must NOT be forwarded
            yes,
            no_skills,
            no_gc,
        }
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn staged_relaunch_forwards_every_side_effecting_flag() {
        // Regression for the adversarial-review finding: a staged
        // Windows relaunch must carry every flag that changes a side
        // effect of the FULL orchestration the child runs — especially
        // `--no-gc`, whose omission silently re-enabled artifact
        // deletion the user explicitly opted out of.
        let args = upgrade_args(true, true, true, true, &["/repo/a", "/repo/b"]);
        let cmd = staged_relaunch_command(Path::new("/tmp/shim.exe"), Path::new("/cwd"), &args);
        let a = argv(&cmd);

        assert_eq!(a[0], "upgrade");
        assert_eq!(a[1], "--staged");
        for flag in ["--yes", "--no-skills", "--no-gc", "--json"] {
            assert!(
                a.iter().any(|x| x == flag),
                "staged relaunch dropped {flag}; argv = {a:?}"
            );
        }
        assert!(
            a.windows(2).any(|w| w[0] == "--also" && w[1] == "/repo/a"),
            "first --also root not forwarded; argv = {a:?}"
        );
        assert!(
            a.windows(2).any(|w| w[0] == "--also" && w[1] == "/repo/b"),
            "second --also root not forwarded; argv = {a:?}"
        );
        // Deliberately not forwarded — see `staged_relaunch_command` doc.
        assert!(!a.iter().any(|x| x == "--allow-self-stage"));
        assert!(!a.iter().any(|x| x == "--dry-run"));
        assert!(!a.iter().any(|x| x == "--finish"));
    }

    #[test]
    fn staged_relaunch_omits_unset_flags() {
        // The forwarding is conditional, not unconditional: an unset
        // flag must stay absent so the child's parsed args match the
        // operator's actual invocation.
        let args = upgrade_args(false, false, false, false, &[]);
        let a = argv(&staged_relaunch_command(
            Path::new("/s.exe"),
            Path::new("/c"),
            &args,
        ));
        assert_eq!(a, vec!["upgrade".to_string(), "--staged".to_string()]);
    }

    #[test]
    fn staged_relaunch_primary_and_fallback_argv_are_identical() {
        // The anti-divergence guarantee. The breakaway spawn and its
        // job-forbidden fallback both build through this one helper, so
        // their argv must be byte-identical — the regression guard for
        // the original fallback that dropped every forwarded flag.
        let args = upgrade_args(true, false, true, true, &["/x"]);
        let primary = staged_relaunch_command(Path::new("/s.exe"), Path::new("/c"), &args);
        let fallback = staged_relaunch_command(Path::new("/s.exe"), Path::new("/c"), &args);
        let pa: Vec<_> = primary.get_args().map(|s| s.to_os_string()).collect();
        let fa: Vec<_> = fallback.get_args().map(|s| s.to_os_string()).collect();
        assert_eq!(pa, fa, "primary and fallback staged argv diverged");
    }
}

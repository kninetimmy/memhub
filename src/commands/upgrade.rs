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
//!
//! ## Windows locked destination (a second, unrelated process)
//!
//! The staging hop above only frees *the orchestrator's own* image lock.
//! It does nothing when some completely different running memhub
//! process — e.g. a live `memhub` MCP server attached to an agent
//! session — holds `~/.cargo/bin/memhub.exe` open: that process is not
//! going anywhere just because we relaunched ourselves elsewhere, so
//! `cargo install` fails every retry (as it did on 2026-07-13, all 3
//! staged attempts). Fix: Windows permits *renaming* a file that is
//! open/mapped for execution — only deleting or overwriting it in place
//! is blocked — which is exactly the manual workaround that resolved
//! that incident. So immediately before the first `cargo install`
//! attempt, `cargo_install_with_retry` unconditionally renames an
//! existing install destination aside to `memhub.exe.old-<timestamp>`
//! (`rename_locked_dest_aside`) whether or not anything is actually
//! holding it; cargo install then simply creates a fresh file at the
//! now-empty path. This is not a lock probe — the rename itself always
//! succeeds, locked or not — so leftovers are bounded by sweeping on
//! every run: `orchestrate_phase` first best-effort deletes `.old-*`
//! files left by earlier runs (`sweep_stale_old_exes`, skipping any
//! still open), then reports whatever is renamed-aside or still left
//! over in the upgrade summary. Never fatal, and Unix is untouched (the
//! rename/sweep pair only runs under `cfg!(windows)`).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::commands::install_manifest::{self, InstallManifest};
use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::util::sha256_hex;
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
    /// Report the outcome of the most recent upgrade from
    /// `~/.memhub/last_upgrade.json` and exit 0/1/3. Does not rebuild.
    pub verify_last: bool,
}

/// Internal parent->child handoff for the skill-resync result. The skill
/// sync runs in the *orchestrate* phase (the old binary, in the source
/// repo where `templates/` lives) but is rendered by the re-exec'd
/// `--finish` child alongside the migrate table so there is one output
/// surface. A hidden CLI flag would be the `--finish` precedent, but
/// this is a pure internal IPC blob, not a user knob, so it travels by
/// env var (same spirit as the test seams).
const SKILLS_ENV: &str = "MEMHUB_UPGRADE_SKILLS_JSON";

/// Companion to `SKILLS_ENV`: the resync's orphan list (files memhub
/// previously installed but no longer ships) crosses the same
/// orchestrate->finish process boundary. Kept as a separate var so the
/// existing `SKILLS_ENV` payload shape is unchanged — an older binary
/// that never sets this simply yields "no orphans this run", which
/// deserializes to an empty list (fail-safe, never fatal).
const ORPHANS_ENV: &str = "MEMHUB_UPGRADE_ORPHANS_JSON";

/// Set to "1" across the orchestrate->finish boundary when the resync hit
/// the first-run (empty-manifest) case and left a pre-existing file
/// untouched, so `--finish` renders the one-time adoption notice. Absent
/// (e.g. an older orchestrate binary, or nothing protected) => no notice.
const FIRSTRUN_ENV: &str = "MEMHUB_UPGRADE_RESYNC_FIRSTRUN";

/// Windows only: crosses the orchestrate->finish process boundary with
/// the abbreviated paths of any `memhub.exe.old-*` install destination
/// left behind this run — one this run itself renamed aside
/// (`rename_locked_dest_aside`), plus any from earlier runs that
/// `sweep_stale_old_exes` could not remove because something still has
/// them open. Absent/empty => nothing to report. JSON-encoded `Vec<String>`,
/// same shape as `SKILLS_ENV`/`ORPHANS_ENV`.
const OLD_EXE_ENV: &str = "MEMHUB_UPGRADE_OLD_EXE_JSON";

/// Exit code from a Windows staged handoff: the real upgrade continues in
/// a detached staged copy, so the invoking shell can't yet know the
/// outcome. It gets this "handed off, pending" code instead of a
/// success/failure it cannot vouch for; `--verify-last` resolves it (F6).
const EXIT_HANDED_OFF: i32 = 3;

/// Terminal state of an upgrade, recorded in `~/.memhub/last_upgrade.json`
/// so `--verify-last` can report 0 (ok) / 1 (failed) / 3 (pending) even
/// when the invoking shell only ever saw the staged relaunch's exit code.
#[derive(Debug, Clone, Copy)]
enum UpgradeReportState {
    /// Handed off / started, no terminal result recorded yet.
    Pending,
    Ok,
    Failed,
}

impl UpgradeReportState {
    fn as_str(self) -> &'static str {
        match self {
            UpgradeReportState::Pending => "pending",
            UpgradeReportState::Ok => "ok",
            UpgradeReportState::Failed => "failed",
        }
    }
}

pub fn run(cwd: &Path, args: UpgradeArgs) -> Result<()> {
    if args.verify_last {
        return verify_last();
    }
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
            write_last_upgrade(
                UpgradeReportState::Pending,
                "upgrade started; completion not yet recorded",
            );
            match orchestrate_phase(cwd, &args) {
                Ok(()) => Ok(()),
                Err(e) => {
                    let msg = format!("upgrade failed before the migrate phase: {e}");
                    write_last_upgrade(UpgradeReportState::Failed, &msg);
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
             note the invoking shell will then receive exit code 3 \
             (handed off; result pending) — poll `memhub upgrade \
             --verify-last`, or read the final 'memhub upgrade:' line."
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

/// The two files `cargo install --path . --force --locked` overwrites: its build
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
         this binary. This shell returns now with exit code {EXIT_HANDED_OFF} \
         (handed off; result pending). Watch the staged run's final \
         'memhub upgrade:' line, or poll with `memhub upgrade --verify-last`.",
        shim.display()
    );
    // Do NOT wait: staying alive keeps our image locked and re-creates the
    // very bug we are fixing. The exit *code* doesn't gate the lock release
    // (process death does), so report the honest "handed off, pending"
    // state instead of a success this process cannot vouch for (F6).
    std::process::exit(EXIT_HANDED_OFF);
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

// ---------------------------------------------------------------------
// Windows locked-destination rename-aside
// ---------------------------------------------------------------------
//
// See the module-level "Windows locked destination" doc above for the
// scenario this handles: a SECOND, unrelated memhub process (not this
// orchestrator — the staging hop above already keeps this process's own
// image out of cargo's conflict set) holding the install destination
// open. Retrying `cargo install` cannot fix that; renaming the
// destination aside can.

/// Filename prefix for a renamed-aside install destination. Distinct
/// from `sweep_stale_staging`'s `memhub-upgrade-*` shim prefix so the two
/// sweeps can never collide.
fn old_exe_prefix() -> String {
    format!("{}.old-", bin_name())
}

/// Pure naming logic, split out so it is unit-testable without touching
/// the filesystem or the (time-dependent) real `now_stamp()`.
fn old_exe_name(stamp: u128) -> String {
    format!("{}{stamp}", old_exe_prefix())
}

/// Selection predicate for the stale-sweep: true for a filename this
/// module could have renamed a destination to. Split out so the
/// selection logic is unit-testable independent of directory scanning.
fn is_old_exe_name(name: &str) -> bool {
    name.starts_with(&old_exe_prefix())
}

/// Rename an existing install destination aside to
/// `<bin>.old-<timestamp>`, returning the new path on success. Not a lock
/// probe: Windows permits renaming a file that is open/mapped for
/// execution (only deleting/overwriting it in place is blocked), so this
/// succeeds whether or not anything currently holds `cargo_bin` open —
/// the proven manual workaround, automated. A missing destination (the
/// very first install) or a rename failure (e.g. a read-only mount) is a
/// silent no-op: `cargo install` proceeds and surfaces its own error if
/// something else is actually wrong.
fn rename_locked_dest_aside(cargo_bin: &Path) -> Option<PathBuf> {
    if !cargo_bin.exists() {
        return None;
    }
    let old = cargo_bin.with_file_name(old_exe_name(now_stamp()));
    std::fs::rename(cargo_bin, &old).ok().map(|_| old)
}

/// Best-effort removal of `.old-*` install destinations left by
/// `rename_locked_dest_aside` on an earlier run. Returns the ones that
/// could NOT be removed (still open — skipped, not retried; reclaimed on
/// some later run once nothing maps them). Unlike `sweep_stale_staging`'s
/// hour cutoff (which guards against racing a concurrent upgrade's live
/// shim), a renamed-aside `.old` file plays no active role once created,
/// so every run simply tries to delete every one it finds; a deletion
/// failure IS the "still locked" signal, not a reason to wait.
fn sweep_stale_old_exes(cargo_bin: &Path) -> Vec<PathBuf> {
    let Some(dir) = cargo_bin.parent() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut leftover = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        if !is_old_exe_name(&name.to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        if std::fs::remove_file(&path).is_err() {
            leftover.push(path);
        }
    }
    leftover
}

/// Run `cargo install --path . --force --locked`, streaming cargo's own output.
/// When `staged` (Windows staged run), retry a failed attempt a few
/// times with backoff: the only expected transient is the parent's
/// image lock lingering past its exit, and cargo's cache makes retries
/// cheap. Non-staged / Unix keeps the original single-shot fail-fast.
///
/// Windows only, and orthogonal to the `staged` retry above: before the
/// first attempt, `rename_locked_dest_aside` unconditionally renames an
/// existing install destination aside. This is what lets THIS run
/// succeed when some OTHER running memhub process (not this
/// orchestrator) still has the destination open — retrying alone never
/// resolves that, since the holder isn't going anywhere. On success,
/// returns the renamed-aside path (if any) so the caller can report it.
fn cargo_install_with_retry(cwd: &Path, staged: bool, cargo_bin: &Path) -> Result<Option<PathBuf>> {
    cargo_install_with_retry_for(cwd, staged, cargo_bin, cfg!(windows), "cargo")
}

/// Same as `cargo_install_with_retry`, with two extra explicit parameters
/// production always fixes but tests can override, for the same reason
/// `stage_decision` takes `is_windows` as a parameter rather than reading
/// `cfg!` internally:
/// - `rename_aside`: whether to rename an existing destination aside
///   before the first attempt (production: `cfg!(windows)`) — lets a
///   test force the Windows rename-aside behavior, including the
///   on-failure restore below, on any host.
/// - `cargo_program`: the program `Command::new` launches (production:
///   `"cargo"`) — lets a test point at a path that cannot possibly
///   spawn, deterministically exercising the launch-failure branch
///   below without touching `PATH` (process-global, so unsafe to mutate
///   from a test in this multi-threaded harness) or needing a real
///   missing-cargo host.
fn cargo_install_with_retry_for(
    cwd: &Path,
    staged: bool,
    cargo_bin: &Path,
    rename_aside: bool,
    cargo_program: &str,
) -> Result<Option<PathBuf>> {
    let renamed_old = if rename_aside {
        rename_locked_dest_aside(cargo_bin)
    } else {
        None
    };

    let attempts = if staged { 3 } else { 1 };
    for attempt in 1..=attempts {
        // NB: a launch failure (cargo itself missing/unspawnable) returns
        // immediately, same as the original `?`-propagation — it is not
        // retried, since there is nothing transient about "cargo is not
        // on PATH". But it must still go through the same aside-restore
        // as an exhausted-retries build failure: `renamed_old` was
        // already moved before this loop started, so this path can
        // strand it exactly like a failed build can (this is what #122's
        // review round caught — the original code let `?` skip the
        // restore here).
        let status = match Command::new(cargo_program)
            .arg("install")
            .arg("--path")
            .arg(".")
            .arg("--force")
            .arg("--locked")
            .current_dir(cwd)
            .status()
        {
            Ok(status) => status,
            Err(e) => {
                return Err(install_failure(
                    cargo_bin,
                    renamed_old,
                    format!("could not launch cargo ({e}); is it on PATH?"),
                ));
            }
        };
        if status.success() {
            return Ok(renamed_old);
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
    Err(install_failed_after_retries(
        cargo_bin,
        renamed_old,
        attempts,
    ))
}

/// Build the terminal error once every install attempt has failed. A
/// source tree that fails to build is expected and recoverable (fix the
/// tree, re-run) — but if `rename_locked_dest_aside` already moved the
/// PRIOR working binary out of the way before the first attempt
/// (`renamed_old` is `Some`), leaving it there on failure means the
/// machine has NO memhub binary on PATH at all: every repo's MCP server
/// and CLI break, and there is no `memhub` left on PATH to even re-run
/// `memhub upgrade`. So put it straight back. If the restore itself
/// fails too (e.g. the aside file was itself removed, or the directory
/// went read-only in between), fall back to naming the exact aside path
/// and the manual command that restores it — never a silent "figure it
/// out yourself". Thin wrapper over `install_failure` so this keeps its
/// existing "build/install failed after N attempt(s)" wording (and
/// existing tests) while sharing the restore-or-report logic with the
/// launch-failure path below.
fn install_failed_after_retries(
    cargo_bin: &Path,
    renamed_old: Option<PathBuf>,
    attempts: i32,
) -> MemhubError {
    install_failure(
        cargo_bin,
        renamed_old,
        format!("build/install failed after {attempts} attempt(s)"),
    )
}

/// Shared terminal-error builder for every way `cargo_install_with_retry_for`
/// can give up — an exhausted retry loop (`install_failed_after_retries`) or
/// cargo itself failing to launch at all (e.g. not on PATH). `reason`
/// describes which one, without a trailing period/semicolon; this appends
/// the "not migrating instances" / restore outcome and formats the whole
/// `MemhubError`. See `install_failed_after_retries`'s doc for why the
/// restore matters at all.
fn install_failure(cargo_bin: &Path, renamed_old: Option<PathBuf>, reason: String) -> MemhubError {
    const COMMAND: &str = "cargo install --path . --force --locked";
    let Some(old) = renamed_old else {
        return MemhubError::ExternalCommand {
            command: COMMAND.to_string(),
            stderr: format!("{reason}; not migrating instances"),
        };
    };
    match std::fs::rename(&old, cargo_bin) {
        Ok(()) => MemhubError::ExternalCommand {
            command: COMMAND.to_string(),
            stderr: format!(
                "{reason}; restored the prior binary (renamed aside before this \
                 attempt) back to {} so memhub is still on PATH; not migrating \
                 instances",
                cargo_bin.display()
            ),
        },
        Err(e) => MemhubError::ExternalCommand {
            command: COMMAND.to_string(),
            stderr: format!(
                "{reason}; the prior binary renamed aside to {} could NOT be \
                 restored to {} ({e}) — no memhub binary is on PATH; restore it \
                 manually: mv \"{}\" \"{}\"",
                old.display(),
                cargo_bin.display(),
                old.display(),
                cargo_bin.display()
            ),
        },
    }
}

// ---------------------------------------------------------------------
// Phase 1: orchestration (old binary)
// ---------------------------------------------------------------------

fn orchestrate_phase(cwd: &Path, args: &UpgradeArgs) -> Result<()> {
    ensure_source_repo(cwd)?;
    let cargo_bin = cargo_bin_path()?;

    // CARGO_INSTALL_ROOT / cargo config `install.root` redirect where
    // `cargo install` actually writes; `cargo_bin_path()` above knows
    // nothing about either, so step 4's `--finish` re-exec would target
    // whatever STALE binary already sits at the assumed path rather than
    // the one just built, running migrate + verify under old code.
    // Refuse loudly rather than risk that silently — before dry-run too,
    // so a preview surfaces the same problem instead of a clean report
    // that a real run would then contradict.
    if let Some(reason) = install_root_override(cwd) {
        return Err(MemhubError::InvalidInput(format!(
            "cargo's install destination is overridden ({reason}); `memhub upgrade` \
             assumes cargo installs to {} and re-execs that exact path for the \
             migrate + verify pass. With an override in effect it would silently \
             re-exec whatever binary already happens to be sitting there instead \
             of the one `cargo install` just built. Refusing rather than risk \
             running migrate/verify under stale code; unset the override (or run \
             `cargo install` yourself and skip `memhub upgrade`'s orchestration) to \
             proceed.",
            cargo_bin.display()
        )));
    }

    if args.dry_run {
        return dry_run_report(cwd, args, &cargo_bin);
    }

    // 0. Windows only: best-effort sweep of `.old-*` install destinations
    //    a PRIOR run left behind via the rename-aside below (see the
    //    module-level "Windows locked destination" doc). Never fatal — a
    //    file still open (e.g. a long-running memhub MCP server) is
    //    simply skipped, not retried, and reported alongside anything
    //    renamed aside this run so leftovers never silently accumulate.
    let mut old_exe_leftovers: Vec<PathBuf> = if cfg!(windows) {
        sweep_stale_old_exes(&cargo_bin)
    } else {
        Vec::new()
    };

    // 1. Rebuild + install. Stream cargo's own output; abort on failure
    //    rather than half-upgrade. On a Windows staged run the original
    //    process may still be releasing its image lock for a few ms, so
    //    retry: cargo's incremental cache makes a retry cheap (a lock
    //    failure skips compile entirely and just re-attempts the move).
    //    On Windows, `cargo_install_with_retry` also unconditionally
    //    renames aside an existing destination before the first attempt
    //    — the fix for a SECOND, unrelated memhub process (not this
    //    orchestrator) holding it open, which retrying alone cannot
    //    resolve.
    println!("==> cargo install --path . --force --locked");
    let renamed_old = cargo_install_with_retry(cwd, args.staged, &cargo_bin)?;
    if let Some(old) = renamed_old {
        println!(
            "==> windows: install destination was in use by another \
             process; renamed aside -> {} (reclaimed on a later upgrade \
             once nothing has it open)",
            old.display()
        );
        old_exe_leftovers.push(old);
    }
    println!("    installed -> {}", cargo_bin.display());

    // 2. One-time, order-independent PATH-shadow fix (closes task 39).
    //    Degrade an IO failure to a warning rather than aborting: the new
    //    binary is already installed by this point, so a failed shadow
    //    repoint must not fail the whole upgrade (U7). Same never-fatal
    //    posture as the skill resync and registry writes below.
    let outcome = shadow_or_warn(fix_path_shadow(&cargo_bin, args.yes));
    if outcome.warn {
        println!("==> PATH: warning: {}", outcome.message);
    } else {
        println!("==> PATH: {}", outcome.message);
    }

    // 3. Resync installed agent skill wrappers from templates/ (decision
    //    97). Additive, idempotent, best-effort — never fatal. Done here
    //    in the old binary because the source repo's `templates/` must
    //    be present (already an `upgrade` precondition); the result is
    //    handed to the re-exec'd child so it renders in one table.
    let resync = if args.no_skills {
        ResyncReport::skipped_all("--no-skills")
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
    if let Ok(js) = serde_json::to_string(&resync.agents) {
        child.env(SKILLS_ENV, js);
    }
    if let Ok(js) = serde_json::to_string(&resync.orphans) {
        child.env(ORPHANS_ENV, js);
    }
    if resync.first_run_hint {
        child.env(FIRSTRUN_ENV, "1");
    }
    if !old_exe_leftovers.is_empty() {
        let abbrev_paths: Vec<String> = old_exe_leftovers.iter().map(|p| abbrev(p)).collect();
        if let Ok(js) = serde_json::to_string(&abbrev_paths) {
            child.env(OLD_EXE_ENV, js);
        }
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
        // A signal-killed child (`.code()` is None on Unix when it died to
        // a signal) must NOT read as success — map the unknown to failure
        // rather than 0 (F6).
        .unwrap_or(1);
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

/// Enumerate the machine-global registry's known roots, **degrading a
/// corrupt or unreadable registry to an empty list plus a warning**
/// instead of aborting the whole upgrade (U7). Per-repo migrate failures
/// already never abort the others; a busted global registry gets the same
/// posture — the source repo and any `--also` roots still upgrade, we just
/// cannot enumerate the rest. Returning an empty list here is strictly
/// safer than propagating: the alternative was aborting *after* the new
/// binary was already installed.
pub fn known_projects_or_warn(warnings: &mut Vec<String>) -> Vec<db::registry::KnownProject> {
    match db::registry::list_known() {
        Ok(known) => known,
        Err(e) => {
            warnings.push(format!(
                "registry unreadable ({e}); continuing with the source repo \
                 and any --also roots only"
            ));
            Vec::new()
        }
    }
}

fn finish_phase(cwd: &Path, args: &UpgradeArgs) -> Result<()> {
    let head = db::latest_schema_version().to_string();
    let mut warnings: Vec<String> = Vec::new();

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
    for kp in known_projects_or_warn(&mut warnings) {
        push(&kp.root_path, &mut roots, &mut seen);
    }
    for p in &args.also {
        push(p, &mut roots, &mut seen);
    }

    let mut reports: Vec<InstanceReport> = Vec::new();
    let mut removed_stale_files: Vec<String> = Vec::new();
    for root in &roots {
        for removed in cleanup_stale_sync_md_twins(root) {
            removed_stale_files.push(format!("{}: {}", abbrev(root), abbrev(&removed)));
        }
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
    // Companion payload: files memhub previously installed but no longer
    // ships. Reported, never deleted. Absent/unparseable => no orphans.
    let orphans: Vec<String> = std::env::var(ORPHANS_ENV)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // First-run adoption notice flag (set by the orchestrate phase).
    let first_run = std::env::var(FIRSTRUN_ENV).ok().as_deref() == Some("1");
    // Windows only: leftover `.old-*` install destinations (renamed
    // aside this run, or still left over from an earlier one) — see the
    // module-level "Windows locked destination" doc. Absent/unparseable
    // (e.g. Unix, or `--finish` invoked directly) => nothing to report.
    let old_exe_leftovers: Vec<String> = std::env::var(OLD_EXE_ENV)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // Best-effort audit-md nag (Wave 2 C7, issue #33): unlike the skill
    // resync/gc above, this needs no old-vs-new-binary IPC — it's a
    // read-only lint over `cwd`'s own CLAUDE.md/AGENTS.md with no schema
    // dependency, so it runs directly here under the freshly installed
    // binary (the same reasoning migrate + verify already run here).
    let audit_nag = check_audit_md(cwd);

    emit(
        &reports,
        pruned,
        &skills,
        &orphans,
        &old_exe_leftovers,
        &removed_stale_files,
        &warnings,
        first_run,
        &audit_nag,
        args.json,
    );

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
    write_last_upgrade(
        if ok {
            UpgradeReportState::Ok
        } else {
            UpgradeReportState::Failed
        },
        &summary,
    );
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
/// exit 3) can still poll the real result via `--verify-last`.
/// Best-effort: a write failure must not turn a good upgrade into a
/// reported failure.
fn write_last_upgrade(state: UpgradeReportState, summary: &str) {
    let Ok(home) = db::home_dir() else { return };
    let dir = home.join(".memhub");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let payload = json!({
        // `ok` retained for older readers; `state` is authoritative and
        // distinguishes "pending" from "failed" (both were `ok:false`).
        "ok": matches!(state, UpgradeReportState::Ok),
        "state": state.as_str(),
        "summary": summary,
        "unix_ms": now_stamp(),
    });
    let _ = std::fs::write(dir.join("last_upgrade.json"), payload.to_string());
}

/// `memhub upgrade --verify-last`: report the most recent upgrade's
/// outcome from `~/.memhub/last_upgrade.json` and exit 0 (ok) / 1
/// (failed or no record) / 3 (handed off, still pending). This is how a
/// caller resolves a staged Windows run whose shell only saw exit 3.
fn verify_last() -> Result<()> {
    let path = db::home_dir()?.join(".memhub").join("last_upgrade.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => {
            println!(
                "memhub upgrade --verify-last: no upgrade recorded at {}",
                path.display()
            );
            std::process::exit(1);
        }
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("(no summary)");
    // Prefer the explicit `state`; fall back to the legacy `ok` bool for a
    // file written by an older binary (it never carried "pending").
    let state = parsed.get("state").and_then(|v| v.as_str()).unwrap_or(
        match parsed.get("ok").and_then(|v| v.as_bool()) {
            Some(true) => "ok",
            _ => "failed",
        },
    );
    match state {
        "ok" => {
            println!("memhub upgrade: SUCCESS — {summary}");
            std::process::exit(0);
        }
        "pending" => {
            println!("memhub upgrade: PENDING — {summary}");
            std::process::exit(EXIT_HANDED_OFF);
        }
        _ => {
            println!("memhub upgrade: FAILED — {summary}");
            std::process::exit(1);
        }
    }
}

/// The retired `sync_md` channel's exact leading line (see the old
/// `render_managed_body`), used as a content marker so this cleanup only
/// ever removes a file it can positively identify as its own generated
/// output — never a coincidental hand-authored file some other tool
/// happens to have written at the same path.
const STALE_SYNC_MD_MARKER: &str = "# Project state (machine-local, generated by memhub)";

/// One-time per-repo cleanup (audit C5 / task 119): the retired `sync_md`
/// channel used to write `AGENTS.md`/`CLAUDE.md` twins into
/// `.memhub/rendered/`, the same directory `render` uses for
/// `PROJECT.md`/`PROJECT_LEDGER.md`. Claude Code auto-loads any nested
/// `CLAUDE.md` it finds, so a stale twin left behind after the channel's
/// removal is not just clutter — it is a live, unmaintained file still
/// shaping an agent's context. Ceasing to write more of them is not
/// enough; a pre-existing one must be actively deleted. Best-effort and
/// idempotent: a missing file, an unreadable one, or a removal failure are
/// all silently skipped rather than turning a routine upgrade into an
/// error over stale-file bookkeeping.
fn cleanup_stale_sync_md_twins(root: &Path) -> Vec<PathBuf> {
    let rendered_dir = root.join(".memhub").join("rendered");
    ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .filter_map(|name| {
            let path = rendered_dir.join(name);
            let contents = std::fs::read_to_string(&path).ok()?;
            if !contents.starts_with(STALE_SYNC_MD_MARKER) {
                return None;
            }
            std::fs::remove_file(&path).ok()?;
            Some(path)
        })
        .collect()
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
            surface: None,
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------
// PATH-shadow fix (order-independent; closes task 39)
// ---------------------------------------------------------------------

/// The mechanism `relink` actually used. Symlinks are preferred, but on
/// Windows they need privilege, so a copy is the honest fallback — and the
/// message must say "copy", not lie and claim "symlink" (U6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Symlink,
    // Only constructed in relink()'s #[cfg(windows)] arm, so a non-Windows
    // lib build never constructs it — silence the resulting dead-code lint.
    #[cfg_attr(not(windows), allow(dead_code))]
    Copy,
}

impl LinkKind {
    fn word(self) -> &'static str {
        match self {
            LinkKind::Symlink => "symlink",
            LinkKind::Copy => "copy",
        }
    }
}

struct ShadowOutcome {
    message: String,
    /// True when this outcome is a degraded IO failure (U7) surfaced as a
    /// warning rather than a successful result. The upgrade continues.
    warn: bool,
}

impl ShadowOutcome {
    fn msg(s: impl Into<String>) -> Self {
        Self {
            message: s.into(),
            warn: false,
        }
    }

    fn warn(s: impl Into<String>) -> Self {
        Self {
            message: s.into(),
            warn: true,
        }
    }
}

/// `relink` succeeded — describe replacing a stale copied binary, naming
/// the **actual** mechanism used (task 39 fix + U6 honesty).
fn stale_replaced_msg(cargo_bin: &Path, kind: LinkKind) -> String {
    format!(
        "replaced stale ~/.local/bin/memhub with {} -> {} (task 39 fixed)",
        kind.word(),
        cargo_bin.display()
    )
}

/// `relink` succeeded — describe repointing an existing symlink, naming
/// the mechanism actually used (a Windows fallback repoint is a copy).
fn repointed_msg(cargo_bin: &Path, kind: LinkKind) -> String {
    format!(
        "repointed ~/.local/bin/memhub -> {} ({})",
        cargo_bin.display(),
        kind.word()
    )
}

/// Degrade a PATH-shadow fix failure to a warning outcome (U7). The new
/// binary is already installed before the shadow step runs, so an IO
/// failure here must be reported, not aborted. Pure `Result ->
/// ShadowOutcome` so the degrade is unit-testable without provoking a real
/// filesystem error.
fn shadow_or_warn(res: Result<ShadowOutcome>) -> ShadowOutcome {
    match res {
        Ok(outcome) => outcome,
        Err(e) => ShadowOutcome::warn(format!(
            "shadow fix skipped ({e}); the new binary is installed — if \
             ~/.local/bin/memhub shadows it, repoint it manually: \
             ln -sf ~/.cargo/bin/memhub ~/.local/bin/memhub"
        )),
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
        let kind = relink(cargo_bin, &shadow)?;
        return Ok(ShadowOutcome::msg(repointed_msg(cargo_bin, kind)));
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
    let kind = relink(cargo_bin, &shadow)?;
    Ok(ShadowOutcome::msg(stale_replaced_msg(cargo_bin, kind)))
}

/// Point `shadow` at `cargo_bin`, returning the **mechanism actually
/// used** so the caller reports the truth (U6). Prefers a symlink; on
/// Windows, where symlinks need privilege, falls back to a copy — and says
/// so, instead of the old code's flat (often false) "symlink" claim.
fn relink(cargo_bin: &Path, shadow: &Path) -> Result<LinkKind> {
    if let Some(parent) = shadow.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(shadow);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(cargo_bin, shadow)?;
        Ok(LinkKind::Symlink)
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(cargo_bin, shadow) {
            Ok(()) => Ok(LinkKind::Symlink),
            Err(_) => {
                // Symlinks need privilege on Windows; a copy is the boring
                // fallback (re-run `memhub upgrade` after each install).
                std::fs::copy(cargo_bin, shadow)?;
                Ok(LinkKind::Copy)
            }
        }
    }
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
    let mut warnings: Vec<String> = Vec::new();

    let mut roots: Vec<PathBuf> = vec![cwd.to_path_buf()];
    for kp in known_projects_or_warn(&mut warnings) {
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
    let resync = if args.no_skills {
        ResyncReport::skipped_all("--no-skills")
    } else {
        sync_skills(cwd, true)
    };
    // Read-only regardless of `dry`, so the preview IS the real check
    // (issue #33: "`--dry-run` reports whether it *would* nag").
    let audit_nag = check_audit_md(cwd);

    if args.json {
        println!(
            "{}",
            json!({
                "dry_run": true,
                "would_run": "cargo install --path . --force --locked",
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
                "skills": resync.agents,
                // Resync orphans (U6): files memhub installed before but no
                // longer ships. Reported, never deleted.
                "resync_orphans": resync.orphans,
                // Degrade warnings (U7), e.g. a corrupt registry.
                "warnings": warnings,
                // First-run adoption notice (U6).
                "resync_first_run_notice": resync.first_run_hint,
                // Additive field (issue #33): same shape as the real
                // (non-dry) upgrade JSON's "audit_md" field.
                "audit_md": audit_nag,
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
    println!("  would run:    cargo install --path . --force --locked");
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
    for s in &resync.agents {
        println!("  skills:       {}", s.dry_line());
    }
    for orphan in &resync.orphans {
        println!("  orphan:       {orphan} (no longer shipped; left in place)");
    }
    if resync.first_run_hint {
        print_first_run_notice();
    }
    if let Some(line) = audit_nag.nag_line(true) {
        println!("  audit md:     {line}");
    }
    for w in &warnings {
        println!("  warning:      {w}");
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

#[allow(clippy::too_many_arguments)]
fn emit(
    reports: &[InstanceReport],
    pruned: usize,
    skills: &[SkillSync],
    orphans: &[String],
    old_exe_leftovers: &[String],
    removed_stale_files: &[String],
    warnings: &[String],
    first_run: bool,
    audit: &AuditNag,
    as_json: bool,
) {
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
                // Resync orphans (U6): files memhub installed before but no
                // longer ships. Reported, never deleted.
                "resync_orphans": orphans,
                // Windows only: `.old-*` install destinations renamed
                // aside (this run or an earlier one) that still exist —
                // reclaimed on a later run once nothing has them open.
                "old_install_leftovers": old_exe_leftovers,
                // Retired-channel cleanup (audit C5 / task 119): stale
                // `sync_md`-generated AGENTS.md/CLAUDE.md twins under
                // `.memhub/rendered/`, actively deleted this run (unlike
                // resync orphans above, these are not merely left in place —
                // Claude Code auto-loads a nested CLAUDE.md, so a stale one
                // is a live correctness hazard, not harmless clutter).
                "removed_stale_files": removed_stale_files,
                // Degrade warnings (U7): recoverable problems that did NOT
                // abort the upgrade (e.g. a corrupt registry, a shadow-fix
                // IO error).
                "warnings": warnings,
                // First-run adoption notice (U6): true when memhub left a
                // pre-existing, unverifiable file untouched on an empty
                // manifest.
                "resync_first_run_notice": first_run,
                // Additive field (issue #33): the audit-md nag result.
                // Always present, `status` distinguishes clean/findings/warn.
                "audit_md": audit,
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
    for orphan in orphans {
        println!("  orphan: {orphan} (no longer shipped; left in place)");
    }
    for old in old_exe_leftovers {
        println!("  old install: {old} (renamed aside; reclaimed once nothing has it open)");
    }
    for removed in removed_stale_files {
        println!("  removed stale file: {removed} (retired sync_md channel; task 119)");
    }
    if first_run {
        print_first_run_notice();
    }
    if let Some(line) = audit.nag_line(false) {
        println!("  audit md: {line}");
    }
    for w in warnings {
        println!("  warning: {w}");
    }
    println!("  {ready}/{total} instances ready");
}

/// One-time notice (U6, REQUIRED 1b) shown when the first resync consults
/// an empty manifest and leaves a pre-existing, unverifiable file
/// untouched. Explains why and how to adopt a file the user DOES want
/// memhub to manage.
fn print_first_run_notice() {
    println!(
        "  note: some pre-existing skill/command files could not be verified as \
         memhub-written and were left untouched."
    );
    println!(
        "        to let memhub manage one, delete it and re-run `memhub upgrade` \
         (it will reinstall + record it)."
    );
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
    /// Units left untouched because they are the user's own file, not one
    /// memhub wrote (install-manifest ownership check, U6). `serde(default)`
    /// so a payload from an older orchestrate binary that predates this
    /// field still deserializes in the freshly installed `--finish` child.
    #[serde(default)]
    pub protected: usize,
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
            protected: 0,
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
            SkillSyncStatus::Synced => {
                let base = if dry {
                    format!("would sync {}", self.synced)
                } else {
                    format!("synced {}", self.synced)
                };
                // Surface WHY files were left untouched (e.g. "2 left
                // untouched (pre-existing, unverified owner)"), not a bare
                // "2 protected" the user cannot act on.
                match &self.detail {
                    Some(d) => format!("{base}; {d}"),
                    None => base,
                }
            }
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

/// The full result of a skill/command resync: the per-agent rows plus the
/// cross-agent orphan list — files memhub installed on a previous run but
/// no longer ships. Orphans are **reported, never deleted** (U6): a stale
/// slash-command is harmless, but deleting the user's file would not be.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResyncReport {
    pub agents: Vec<SkillSync>,
    /// `$HOME`-abbreviated paths of orphaned installs, sorted.
    pub orphans: Vec<String>,
    /// True on the first resync that consults an empty manifest AND leaves
    /// at least one pre-existing file untouched (U6, REQUIRED 1b). The
    /// caller surfaces a one-time notice explaining the remedy (delete the
    /// file + re-run) so a memhub-written-but-unprovable file is not
    /// silently frozen at its old version forever.
    #[serde(default)]
    pub first_run_hint: bool,
}

impl ResyncReport {
    fn skipped_all(reason: &str) -> Self {
        ResyncReport {
            agents: vec![SkillSync::skipped_all(reason)],
            orphans: Vec::new(),
            first_run_hint: false,
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
pub fn sync_skills(source_repo: &Path, dry: bool) -> ResyncReport {
    let home = match db::home_dir() {
        Ok(h) => h,
        Err(e) => {
            return ResyncReport {
                agents: vec![SkillSync {
                    agent: "(all)".to_string(),
                    target: String::new(),
                    status: SkillSyncStatus::Warn,
                    synced: 0,
                    protected: 0,
                    detail: Some(format!("cannot resolve home dir: {e}")),
                }],
                orphans: Vec::new(),
                first_run_hint: false,
            };
        }
    };

    // Load the ownership manifest ONCE. Absent/corrupt => empty => every
    // pre-existing target reads as user-owned => nothing is overwritten.
    let manifest = InstallManifest::load();

    let skills_root = source_repo.join("templates").join("skills");
    let commands_root = source_repo.join("templates").join("commands");

    // Install targets are declared up front so each outcome can be paired
    // with its target dir — needed to carry a manifest slice forward when
    // an agent's source could not be read this run (see below).
    let claude_target = home.join(".claude").join("commands");
    let codex_target = home.join(".codex").join("skills");
    let oc_skills_target = home.join(".config").join("opencode").join("skills");
    let oc_commands_target = home.join(".config").join("opencode").join("commands");
    let outcomes: Vec<(AgentOutcome, PathBuf)> = vec![
        (
            sync_one(
                "claude",
                &skills_root.join("claude"),
                &claude_target,
                CopyKind::FlatMd,
                dry,
                &manifest,
            ),
            claude_target.clone(),
        ),
        (
            sync_one(
                "codex",
                &skills_root.join("codex"),
                &codex_target,
                CopyKind::DirPerSkill,
                dry,
                &manifest,
            ),
            codex_target.clone(),
        ),
        (
            sync_one(
                "opencode-skills",
                &skills_root.join("opencode"),
                &oc_skills_target,
                CopyKind::DirPerSkill,
                dry,
                &manifest,
            ),
            oc_skills_target.clone(),
        ),
        (
            sync_one(
                "opencode-commands",
                &commands_root.join("opencode"),
                &oc_commands_target,
                CopyKind::FlatMd,
                dry,
                &manifest,
            ),
            oc_commands_target.clone(),
        ),
    ];

    // Rebuild the manifest from what memhub owns *this* run, and record
    // every target path we ship (owned or not) so orphan detection can
    // tell "no longer shipped" from "shipped but the user's".
    let mut next = InstallManifest::default();
    let mut shipped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Target dirs whose source we could NOT read this run (transient IO):
    // we make no claims about them — their manifest slice is carried
    // forward untouched and nothing under them is orphaned.
    let mut carried_prefixes: Vec<PathBuf> = Vec::new();
    let mut agents = Vec::with_capacity(outcomes.len());
    for (outcome, target_dir) in outcomes {
        if outcome.enumerated {
            for (key, owned) in outcome.shipped {
                shipped.insert(key.clone());
                if let Some(hash) = owned {
                    next.record(key, hash);
                }
            }
        } else {
            // Source unreadable this run: a transient error must never cede
            // ownership. Without this, every still-owned file for this agent
            // would look orphaned and be dropped from `next`, so next run it
            // reads as user-owned and memhub silently stops updating it.
            carried_prefixes.push(target_dir);
        }
        agents.push(outcome.report);
    }

    // Carry the existing manifest slice forward for every un-enumerated
    // agent, so its ownership survives the transient failure.
    for key in manifest.keys() {
        if under_any(key, &carried_prefixes)
            && let Some(hash) = manifest.recorded(key)
        {
            next.record(key.clone(), hash.to_string());
        }
    }

    // Orphans: paths memhub recorded before but no longer ships, still on
    // disk — EXCLUDING anything under a carried (un-enumerated) prefix,
    // which we make no claims about this run. Reported here, NEVER deleted.
    let mut orphans: Vec<String> = manifest
        .keys()
        .filter(|k| !shipped.contains(k.as_str()))
        .filter(|k| !under_any(k, &carried_prefixes))
        .filter(|k| Path::new(k).exists())
        .map(|k| abbrev(Path::new(k)))
        .collect();
    orphans.sort();

    // First-run notice (REQUIRED 1b): the first time memhub consults an
    // empty manifest and leaves at least one pre-existing file untouched,
    // flag it so the caller can explain the remedy — otherwise a
    // memhub-written-but-unprovable file stays frozen with nothing said.
    let protected_total: usize = agents.iter().map(|a| a.protected).sum();
    let first_run_hint = manifest.is_empty() && protected_total > 0;

    // Persist the refreshed ownership record. Best-effort and never fatal
    // (same posture as the registry write): a failure just means memhub
    // conservatively re-derives ownership next run. Only write when memhub
    // actually owns something, so a machine with no agent dirs set up does
    // not get `~/.memhub/` created for an empty manifest.
    if !dry
        && !next.is_empty()
        && let Err(e) = next.save()
    {
        log::debug!("install manifest save skipped: {e}");
    }

    ResyncReport {
        agents,
        orphans,
        first_run_hint,
    }
}

/// True when `key` (a target path string) lives under any of `prefixes`.
/// The four install targets are distinct, non-nested dirs, so this cleanly
/// partitions manifest entries by owning agent.
fn under_any(key: &str, prefixes: &[PathBuf]) -> bool {
    let p = Path::new(key);
    prefixes.iter().any(|prefix| p.starts_with(prefix))
}

/// One agent's resync result: the human-facing row plus, for the manifest
/// and orphan bookkeeping, every target path this agent ships this run.
/// `Some(hash)` => memhub owns it (goes in the refreshed manifest); `None`
/// => shipped but left to the user (or failed) — tracked only so it is not
/// mistaken for an orphan.
struct AgentOutcome {
    report: SkillSync,
    shipped: Vec<(String, Option<String>)>,
    /// True when memhub could authoritatively enumerate what this agent
    /// ships this run (its source dir was read). False when the source read
    /// FAILED — in which case `shipped` is empty but says nothing, so the
    /// caller must carry the manifest slice forward rather than orphaning it.
    enumerated: bool,
}

/// What a resync did with one leaf file.
enum FileAction {
    /// Installed or updated (or, in dry mode, would be).
    Wrote,
    /// Owned and already byte-identical — recorded, no write.
    AlreadyCurrent,
    /// Left untouched: the user's own file, not memhub's.
    Protected,
    /// IO failure reading the template or writing the target.
    Failed(String),
}

/// Outcome of one install unit (a flat `*.md` file, or a whole skill dir).
enum UnitStatus {
    Synced,
    Protected,
    Failed(String),
}

fn sync_one(
    agent: &str,
    src: &Path,
    target: &Path,
    kind: CopyKind,
    dry: bool,
    manifest: &InstallManifest,
) -> AgentOutcome {
    let label = abbrev(target);
    let skip = |status: SkillSyncStatus, detail: Option<String>, enumerated: bool| AgentOutcome {
        report: SkillSync {
            agent: agent.to_string(),
            target: label.clone(),
            status,
            synced: 0,
            protected: 0,
            detail,
        },
        shipped: Vec::new(),
        enumerated,
    };

    // Only sync an agent dir the user actually set up. A missing dir or
    // a non-dir at that path is a clean skip — never created. These ARE
    // authoritative (`enumerated: true`): the target is genuinely gone, so
    // there is nothing to carry forward.
    match std::fs::symlink_metadata(target) {
        Err(_) => {
            return skip(
                SkillSyncStatus::Skipped,
                Some("agent dir absent (not set up)".to_string()),
                true,
            );
        }
        Ok(m) if !m.is_dir() => {
            return skip(
                SkillSyncStatus::Skipped,
                Some("path exists but is not a directory".to_string()),
                true,
            );
        }
        Ok(_) => {}
    }

    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(e) => {
            // Source unreadable: NOT authoritative (`enumerated: false`).
            // Possibly transient, so the caller must carry this agent's
            // manifest slice forward instead of orphaning still-owned files.
            return skip(
                SkillSyncStatus::Warn,
                Some(format!("templates unreadable at {}: {e}", src.display())),
                false,
            );
        }
    };

    let mut synced = 0usize;
    let mut protected = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut shipped: Vec<(String, Option<String>)> = Vec::new();
    for entry in entries.flatten() {
        let from = entry.path();
        let name = entry.file_name();
        if !skill_surface_enabled(&name) {
            continue;
        }
        let (status, leaves) = match kind {
            CopyKind::FlatMd => {
                if from.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if !from.is_file() {
                    continue;
                }
                sync_unit_flat(&from, &target.join(&name), manifest, dry)
            }
            CopyKind::DirPerSkill => {
                if !from.is_dir() {
                    continue;
                }
                sync_unit_dir(&from, &target.join(&name), manifest, dry)
            }
        };
        shipped.extend(leaves);
        match status {
            UnitStatus::Synced => synced += 1,
            UnitStatus::Protected => protected += 1,
            UnitStatus::Failed(e) => errors.push(e),
        }
    }

    let report = if errors.is_empty() {
        // Neutral wording: a protected file may be memhub's own that memhub
        // simply cannot PROVE it wrote (no manifest entry). "the user's own"
        // would be literally false in that case (REQUIRED 1c).
        let detail = (protected > 0)
            .then(|| format!("{protected} left untouched (pre-existing, unverified owner)"));
        SkillSync {
            agent: agent.to_string(),
            target: label,
            status: SkillSyncStatus::Synced,
            synced,
            protected,
            detail,
        }
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
        if protected > 0 {
            detail.push_str(&format!("; {protected} left untouched (unverified owner)"));
        }
        SkillSync {
            agent: agent.to_string(),
            target: label,
            status: SkillSyncStatus::Warn,
            synced,
            protected,
            detail: Some(detail),
        }
    };

    AgentOutcome {
        report,
        shipped,
        enumerated: true,
    }
}

/// Metrics and viz templates stay in the source tree for an explicit
/// reactivation build, but normal upgrades must not install their agent
/// surfaces while the subsystem is hibernated.
fn skill_surface_enabled(name: &std::ffi::OsStr) -> bool {
    let raw = name.to_string_lossy();
    let stem = raw.strip_suffix(".md").unwrap_or(&raw);
    match stem {
        "metrics" => cfg!(feature = "metrics"),
        "viz" => cfg!(feature = "viz"),
        _ => true,
    }
}

/// A flat `*.md` install unit is exactly one file.
fn sync_unit_flat(
    from: &Path,
    to: &Path,
    manifest: &InstallManifest,
    dry: bool,
) -> (UnitStatus, Vec<(String, Option<String>)>) {
    let key = to.to_string_lossy().to_string();
    let (action, owned) = install_one_file(from, to, manifest, dry);
    let status = match &action {
        FileAction::Wrote | FileAction::AlreadyCurrent => UnitStatus::Synced,
        FileAction::Protected => UnitStatus::Protected,
        FileAction::Failed(e) => UnitStatus::Failed(format!("{}: {e}", leaf_label(to))),
    };
    (status, vec![(key, owned)])
}

/// A skill-dir install unit: apply the ownership check to every leaf file
/// so a user's own file *inside* an otherwise-memhub skill dir is still
/// protected. The unit is "protected" if any leaf is the user's, "failed"
/// if any leaf errors, otherwise "synced".
fn sync_unit_dir(
    from: &Path,
    to: &Path,
    manifest: &InstallManifest,
    dry: bool,
) -> (UnitStatus, Vec<(String, Option<String>)>) {
    let mut leaves: Vec<PathBuf> = Vec::new();
    collect_leaves(from, Path::new(""), &mut leaves);

    let mut shipped: Vec<(String, Option<String>)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut any_protected = false;
    for leaf in &leaves {
        let dst = to.join(leaf);
        let key = dst.to_string_lossy().to_string();
        let (action, owned) = install_one_file(&from.join(leaf), &dst, manifest, dry);
        match &action {
            FileAction::Wrote | FileAction::AlreadyCurrent => {}
            FileAction::Protected => any_protected = true,
            FileAction::Failed(e) => errors.push(format!("{}: {e}", leaf.display())),
        }
        shipped.push((key, owned));
    }

    let status = if !errors.is_empty() {
        let shown: Vec<&String> = errors.iter().take(3).collect();
        let more = errors.len().saturating_sub(shown.len());
        let mut d = shown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        if more > 0 {
            d.push_str(&format!("; +{more} more"));
        }
        UnitStatus::Failed(d)
    } else if any_protected {
        UnitStatus::Protected
    } else {
        UnitStatus::Synced
    };
    (status, shipped)
}

/// Apply one template file to its target under the install-manifest
/// ownership rules (U6). Returns the action taken and, when memhub owns
/// the result, the content hash to record in the refreshed manifest.
///
/// This is where the "ours vs the user's file" boundary is enforced at the
/// filesystem: an absent file installs; a present file is (over)written
/// ONLY when `install_manifest::decide` proves it is memhub's; anything
/// else is left exactly as the user has it.
fn install_one_file(
    from: &Path,
    to: &Path,
    manifest: &InstallManifest,
    dry: bool,
) -> (FileAction, Option<String>) {
    let key = to.to_string_lossy().to_string();
    let template = match std::fs::read(from) {
        Ok(b) => b,
        Err(e) => return (FileAction::Failed(format!("read template: {e}")), None),
    };
    // Distinguish "absent" (safe to install) from "present but unreadable"
    // (fail-safe: cannot prove it is ours, so treat it as the user's).
    let on_disk: Option<Vec<u8>> = match std::fs::symlink_metadata(to) {
        Err(_) => None,
        Ok(_) => match std::fs::read(to) {
            Ok(b) => Some(b),
            Err(_) => return (FileAction::Protected, None),
        },
    };
    let template_hash = sha256_hex(&template);
    match install_manifest::decide(manifest.recorded(&key), on_disk.as_deref(), &template) {
        install_manifest::Decision::UserOwned => (FileAction::Protected, None),
        install_manifest::Decision::AlreadyCurrent => {
            (FileAction::AlreadyCurrent, Some(template_hash))
        }
        install_manifest::Decision::Install | install_manifest::Decision::Update => {
            if dry {
                return (FileAction::Wrote, Some(template_hash));
            }
            if let Some(parent) = to.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return (FileAction::Failed(format!("mkdir: {e}")), None);
            }
            match std::fs::write(to, &template) {
                Ok(()) => (FileAction::Wrote, Some(template_hash)),
                Err(e) => (FileAction::Failed(e.to_string()), None),
            }
        }
    }
}

/// Collect every leaf file under `root`, as paths relative to `root`.
fn collect_leaves(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let child = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_leaves(root, &child, out),
            Ok(_) => out.push(child),
            Err(_) => {}
        }
    }
}

fn leaf_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------
// audit md nag (Wave 2 C7, issue #33)
// ---------------------------------------------------------------------

/// Best-effort outcome of running `memhub audit md` as part of `upgrade`.
/// Read-only and always-on (no `--no-*` opt-out — issue #33 acceptance
/// criteria says one isn't required), and never fatal: same posture as
/// `SkillSync` / the target-gc step above. A failure to even run the
/// audit (e.g. `.memhub` not discoverable, config unreadable) degrades
/// to `Warn` rather than propagating an error that would fail the
/// upgrade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditNagStatus {
    /// The audit ran to completion with zero findings.
    Clean,
    /// The audit ran to completion with at least one finding.
    Findings,
    /// The audit itself could not be completed. Degraded, not fatal —
    /// the upgrade still succeeds.
    Warn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditNag {
    pub status: AuditNagStatus,
    /// Finding count when `status == Findings`; `0` otherwise.
    pub count: usize,
    /// Set only when `status == Warn` (the error the audit itself hit).
    pub detail: Option<String>,
}

impl AuditNag {
    fn clean() -> Self {
        AuditNag {
            status: AuditNagStatus::Clean,
            count: 0,
            detail: None,
        }
    }

    fn findings(count: usize) -> Self {
        AuditNag {
            status: AuditNagStatus::Findings,
            count,
            detail: None,
        }
    }

    fn warn(detail: impl Into<String>) -> Self {
        AuditNag {
            status: AuditNagStatus::Warn,
            count: 0,
            detail: Some(detail.into()),
        }
    }

    /// A single nag line, only when there's something to say — a clean
    /// repo prints nothing (issue #33: "prints a single nag line when
    /// findings exist"), rather than adding routine noise to every
    /// upgrade the way the always-printed skills/gc lines do. `dry`
    /// swaps "N finding(s)" for "would flag N finding(s)" to match the
    /// `would migrate` / `would sync` convention `dry_run_report`
    /// already uses for its other previews.
    pub fn nag_line(&self, dry: bool) -> Option<String> {
        match self.status {
            AuditNagStatus::Clean => None,
            AuditNagStatus::Findings if dry => Some(format!(
                "would flag {} finding(s) — see `memhub audit md` (or the /audit-md skill)",
                self.count
            )),
            AuditNagStatus::Findings => Some(format!(
                "{} finding(s) — run `memhub audit md` (or the /audit-md skill) for details",
                self.count
            )),
            AuditNagStatus::Warn => Some(format!(
                "skipped ({})",
                self.detail.as_deref().unwrap_or("audit md failed")
            )),
        }
    }
}

/// Run `memhub audit md` against `cwd` and reduce it to an `AuditNag`.
/// Best-effort: any `Err` from the audit (e.g. `.memhub` not
/// discoverable from `cwd`, config unreadable) becomes
/// `AuditNagStatus::Warn` rather than propagating — same "never fatal"
/// posture as `sync_skills` / `crate::commands::gc::run` above. Always
/// calls with `strict = false`; the exit-code escalation `--strict`
/// provides is irrelevant here, only `findings` is read.
pub fn check_audit_md(cwd: &Path) -> AuditNag {
    match crate::commands::audit_md::run(cwd, false) {
        Ok(report) if report.findings.is_empty() => AuditNag::clean(),
        Ok(report) => AuditNag::findings(report.findings.len()),
        Err(e) => AuditNag::warn(e.to_string()),
    }
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

/// Parse a cargo config file's `[install] root = "..."` (the only key
/// this reads), tolerating any other shape (missing table, missing key,
/// non-string value, invalid TOML) as "not set" rather than an error —
/// this is best-effort detection layered on top of `cargo_bin_path`'s
/// assumption, not a config parser memhub owns or must fully validate.
fn parse_install_root_toml(text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(text).ok()?;
    value
        .get("install")?
        .get("root")?
        .as_str()
        .map(str::to_string)
}

/// `install.root`, if set, from whichever of cargo's two config file
/// names (`config.toml` takes precedence over the legacy `config`, same
/// as cargo itself) exists directly inside `config_dir`.
fn read_install_root_from_config_dir(config_dir: &Path) -> Option<String> {
    for name in ["config.toml", "config"] {
        if let Ok(text) = std::fs::read_to_string(config_dir.join(name))
            && let Some(root) = parse_install_root_toml(&text)
        {
            return Some(root);
        }
    }
    None
}

/// Search cargo's own config precedence order for an `install.root`
/// override of the default install destination: this process's `cwd`
/// and every ancestor's `.cargo/` subdirectory (closest wins, same as
/// cargo's own merge order), then `cargo_home` itself — cargo's config
/// lives directly inside `$CARGO_HOME`, not under an extra `.cargo/`
/// there. Returns `(root value, human-readable source path)` for the
/// first one found, or `None` if nothing sets it. Split out from
/// `install_root_override` so it is unit-testable against plain tempdirs
/// without touching real env vars or `$HOME`.
fn find_install_root_override(cwd: &Path, cargo_home: &Path) -> Option<(String, String)> {
    let mut dir = Some(cwd.to_path_buf());
    while let Some(d) = dir {
        let config_dir = d.join(".cargo");
        if let Some(root) = read_install_root_from_config_dir(&config_dir) {
            return Some((root, config_dir.display().to_string()));
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    read_install_root_from_config_dir(cargo_home).map(|root| {
        let source = cargo_home.display().to_string();
        (root, source)
    })
}

/// Whole-picture check used by `orchestrate_phase` before it touches
/// anything: does some cargo configuration override the install
/// destination `cargo_bin_path` assumes ($CARGO_HOME/bin, or
/// ~/.cargo/bin when `CARGO_HOME` is unset)? `CARGO_INSTALL_ROOT` (env,
/// cargo's own highest-precedence override) is checked first, then
/// cargo config `install.root` via `find_install_root_override`. Either
/// one redirects `cargo install`'s actual destination somewhere
/// `cargo_bin_path` never looks — so the `--finish` re-exec in
/// `orchestrate_phase` (which uses the `cargo_bin_path()` value
/// verbatim, not a PATH lookup, precisely to dodge PATH shadows) would
/// silently re-exec whatever STALE binary is already sitting at that
/// assumed path instead of the one `cargo install` just built, running
/// migrate + verify under old code. `None` means nothing overrides the
/// default and `cargo_bin_path()` can be trusted.
fn install_root_override(cwd: &Path) -> Option<String> {
    if let Some(v) = std::env::var_os("CARGO_INSTALL_ROOT").filter(|s| !s.is_empty()) {
        return Some(format!("CARGO_INSTALL_ROOT={}", PathBuf::from(v).display()));
    }
    let cargo_home = match std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => match db::home_dir() {
            Ok(h) => h.join(".cargo"),
            Err(_) => return None,
        },
    };
    find_install_root_override(cwd, &cargo_home)
        .map(|(root, source)| format!("install.root={root} (from {source})"))
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
            verify_last: false,
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

    // --- U6: relink mechanism honesty --------------------------------

    #[test]
    fn link_kind_word_is_honest() {
        assert_eq!(LinkKind::Symlink.word(), "symlink");
        assert_eq!(LinkKind::Copy.word(), "copy");
    }

    #[test]
    fn stale_replace_message_names_the_real_mechanism() {
        // The exact bug being fixed: the copy fallback used to print
        // "symlink". The message must now match reality.
        let bin = Path::new("/home/u/.cargo/bin/memhub");
        assert!(
            stale_replaced_msg(bin, LinkKind::Symlink).contains("with symlink"),
            "a real symlink should say symlink"
        );
        let copied = stale_replaced_msg(bin, LinkKind::Copy);
        assert!(copied.contains("with copy"), "{copied}");
        assert!(
            !copied.contains("symlink"),
            "a copy fallback must NOT claim symlink: {copied}"
        );
    }

    #[test]
    fn repoint_message_names_the_real_mechanism() {
        let bin = Path::new("/home/u/.cargo/bin/memhub");
        assert!(repointed_msg(bin, LinkKind::Symlink).contains("(symlink)"));
        let copied = repointed_msg(bin, LinkKind::Copy);
        assert!(copied.contains("(copy)"), "{copied}");
        assert!(!copied.contains("symlink"), "{copied}");
    }

    #[test]
    fn relink_returns_the_mechanism_it_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join("memhub-bin");
        std::fs::write(&cargo_bin, b"#!/bin/sh\n").expect("write bin");
        // Parent dir intentionally does not exist yet: relink must create it.
        let shadow = dir.path().join("nested").join("memhub");
        let kind = relink(&cargo_bin, &shadow).expect("relink");
        assert!(
            shadow.exists(),
            "relink must create the shadow at {shadow:?}"
        );
        #[cfg(unix)]
        assert_eq!(kind, LinkKind::Symlink, "unix always symlinks");
        // On Windows the result is Symlink or Copy depending on privilege;
        // either way relink reports what it actually did.
        let _ = kind;
    }

    // --- U7: PATH-shadow IO error degrades, never aborts -------------

    #[test]
    fn shadow_or_warn_degrades_errors_to_a_warning() {
        let degraded = shadow_or_warn(Err(MemhubError::InvalidInput("disk full".to_string())));
        assert!(
            degraded.warn,
            "an error must produce a warn outcome, not abort"
        );
        assert!(
            degraded.message.contains("skipped") && degraded.message.contains("disk full"),
            "the warning must explain what was skipped and why: {}",
            degraded.message
        );
    }

    #[test]
    fn shadow_or_warn_passes_success_through() {
        let ok = shadow_or_warn(Ok(ShadowOutcome::msg("all good")));
        assert!(!ok.warn);
        assert_eq!(ok.message, "all good");
    }

    // --- U6: protected reporting is loud and honest (REQUIRED 1a/1c) --

    #[test]
    fn synced_row_surfaces_protected_detail() {
        let s = SkillSync {
            agent: "claude".to_string(),
            target: "~/.claude/commands".to_string(),
            status: SkillSyncStatus::Synced,
            synced: 10,
            protected: 2,
            detail: Some("2 left untouched (pre-existing, unverified owner)".to_string()),
        };
        let line = s.line();
        assert!(line.contains("synced 10"), "{line}");
        assert!(
            line.contains("left untouched") && line.contains("unverified owner"),
            "a protected summary must explain itself: {line}"
        );
        assert!(
            !line.contains("the user's own"),
            "must not claim ownership it cannot prove: {line}"
        );
    }

    #[test]
    fn synced_row_without_protected_stays_terse() {
        let s = SkillSync {
            agent: "claude".to_string(),
            target: "~/.claude/commands".to_string(),
            status: SkillSyncStatus::Synced,
            synced: 11,
            protected: 0,
            detail: None,
        };
        assert!(s.line().contains("synced 11"));
        assert!(!s.line().contains("untouched"));
    }

    // --- Windows locked-destination rename-aside ---------------------

    #[test]
    fn old_exe_name_uses_bin_name_and_stamp() {
        let name = old_exe_name(1720900123456);
        assert!(
            name.starts_with(bin_name()),
            "expected {name} to start with {}",
            bin_name()
        );
        assert!(name.ends_with(".old-1720900123456"), "{name}");
    }

    #[test]
    fn is_old_exe_name_matches_only_the_rename_aside_pattern() {
        let ours = old_exe_name(1);
        assert!(is_old_exe_name(&ours), "{ours}");
        // The manual workaround's leftover from the 2026-07-13 incident
        // uses a non-numeric suffix; the predicate must not care what
        // follows the prefix, only that it's there.
        assert!(is_old_exe_name(&format!(
            "{}upgrade-20260713",
            old_exe_prefix()
        )));
        // The bare install destination is not itself a leftover.
        assert!(!is_old_exe_name(bin_name()));
        // A different sweep's prefix (the staged %TEMP% shim) must never
        // be picked up by this predicate — the two sweeps must not
        // collide.
        assert!(!is_old_exe_name("memhub-upgrade-1234-5678.exe"));
        assert!(!is_old_exe_name("somethingelse.old-1"));
    }

    #[test]
    fn rename_locked_dest_aside_is_noop_when_destination_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("does-not-exist.exe");
        assert_eq!(rename_locked_dest_aside(&dest), None);
    }

    #[test]
    fn rename_locked_dest_aside_renames_an_existing_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join(bin_name());
        std::fs::write(&dest, b"fake binary").expect("write dest");

        let old = rename_locked_dest_aside(&dest).expect("rename should happen");

        assert!(!dest.exists(), "original destination must be gone");
        assert!(old.exists(), "renamed-aside file must exist");
        assert_eq!(old.parent(), Some(dir.path()));
        let name = old.file_name().unwrap().to_string_lossy().into_owned();
        assert!(is_old_exe_name(&name), "{name}");
        assert_eq!(
            std::fs::read(&old).unwrap(),
            b"fake binary",
            "rename must preserve contents, not truncate/recreate"
        );
    }

    #[test]
    fn sweep_stale_old_exes_removes_matches_and_leaves_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join(bin_name());
        // Two stale renamed-aside files from earlier runs...
        std::fs::write(dir.path().join(old_exe_name(1)), b"a").unwrap();
        std::fs::write(dir.path().join(old_exe_name(2)), b"b").unwrap();
        // ...and files the sweep must never touch: the live destination,
        // an unrelated file, and a staged-shim leftover (swept
        // separately by `sweep_stale_staging`).
        std::fs::write(&dest, b"live").unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), b"c").unwrap();
        std::fs::write(dir.path().join("memhub-upgrade-1-2.exe"), b"d").unwrap();

        let leftover = sweep_stale_old_exes(&dest);

        assert!(
            leftover.is_empty(),
            "nothing here is actually locked; sweep should reclaim it all: {leftover:?}"
        );
        assert!(!dir.path().join(old_exe_name(1)).exists());
        assert!(!dir.path().join(old_exe_name(2)).exists());
        assert!(dest.exists(), "the live destination must be left alone");
        assert!(dir.path().join("unrelated.txt").exists());
        assert!(
            dir.path().join("memhub-upgrade-1-2.exe").exists(),
            "the staged-shim prefix belongs to a different sweep"
        );
    }

    #[test]
    fn sweep_stale_old_exes_on_missing_dir_returns_empty() {
        let dest = std::env::temp_dir()
            .join("memhub-upgrade-tests-definitely-absent-dir-xyz")
            .join(bin_name());
        assert_eq!(sweep_stale_old_exes(&dest), Vec::<PathBuf>::new());
    }

    // --- issue #122: restore the aside copy on a failed install ------

    #[test]
    fn install_failed_after_retries_without_rename_aside_stays_terse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join(bin_name());
        let err = install_failed_after_retries(&cargo_bin, None, 1);
        let msg = err.to_string();
        assert!(msg.contains("failed after 1 attempt"), "{msg}");
        assert!(
            !msg.contains("restore"),
            "nothing was renamed aside; must not talk about restoring: {msg}"
        );
    }

    #[test]
    fn install_failed_after_retries_restores_the_aside_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join(bin_name());
        let old = dir.path().join(old_exe_name(1));
        std::fs::write(&old, b"prior working binary").unwrap();

        let err = install_failed_after_retries(&cargo_bin, Some(old.clone()), 3);

        assert!(
            cargo_bin.exists(),
            "the prior binary must be back at the install destination"
        );
        assert!(!old.exists(), "the aside copy must be gone, moved back");
        assert_eq!(std::fs::read(&cargo_bin).unwrap(), b"prior working binary");
        let msg = err.to_string();
        assert!(msg.contains("restored"), "{msg}");
        assert!(msg.contains(&cargo_bin.display().to_string()), "{msg}");
    }

    #[test]
    fn install_failed_after_retries_reports_manual_command_when_restore_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join(bin_name());
        // The aside path does not exist, so the restore rename must fail —
        // simulating it having vanished out from under this run.
        let old = dir.path().join(old_exe_name(1));

        let err = install_failed_after_retries(&cargo_bin, Some(old.clone()), 1);

        assert!(
            !cargo_bin.exists(),
            "nothing to restore from, so still absent"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("could NOT be restored") || msg.contains("could not be restored"),
            "{msg}"
        );
        assert!(
            msg.contains(&old.display().to_string())
                && msg.contains(&cargo_bin.display().to_string()),
            "the exact aside path and destination must both be named so a human can \
             restore it by hand: {msg}"
        );
        assert!(
            msg.contains("mv ") || msg.contains("mv\""),
            "must print the actual restore command, not just describe it: {msg}"
        );
    }

    #[test]
    fn cargo_install_with_retry_for_restores_aside_copy_on_build_failure() {
        // A cwd with no Cargo.toml makes `cargo install --path .` fail
        // immediately (no network, no real build) — a real, deterministic
        // failed-build path, not a mocked one. `rename_aside = true` is
        // forced explicitly (rather than gated on `cfg!(windows)`) so this
        // exercises the Windows-only production behavior on any host,
        // including this aarch64 Linux CI (see `cargo_install_with_retry_for`'s
        // doc).
        let src = tempfile::tempdir().expect("src tempdir");
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let cargo_bin = bin_dir.path().join(bin_name());
        std::fs::write(&cargo_bin, b"prior working binary").unwrap();

        let result = cargo_install_with_retry_for(src.path(), false, &cargo_bin, true, "cargo");

        assert!(
            result.is_err(),
            "cargo install against a dir with no Cargo.toml must fail"
        );
        assert!(
            cargo_bin.exists(),
            "the prior binary must be restored, not left renamed aside"
        );
        assert_eq!(
            std::fs::read(&cargo_bin).unwrap(),
            b"prior working binary",
            "restore must not corrupt the prior binary's contents"
        );
        // No abandoned `.old-*` file left behind either.
        let leftovers = sweep_stale_old_exes(&cargo_bin);
        assert!(
            leftovers.is_empty(),
            "restore must not leave a renamed-aside file behind: {leftovers:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("restored"), "{msg}");
    }

    #[test]
    fn cargo_install_with_retry_for_restores_aside_copy_when_cargo_cannot_launch() {
        // The residual strand-path caught in #122's review round: a launch
        // failure (`Command::new(cargo_program).status()` itself erroring,
        // e.g. cargo not found) used to `?`-propagate straight out of the
        // retry loop, BEFORE `install_failed_after_retries` was ever
        // reached — so `renamed_old` was silently dropped with no restore
        // and no printed recovery command, stranding the prior binary
        // aside with nothing left on PATH. Point `cargo_program` at a path
        // that cannot possibly spawn (deterministic, no PATH mutation, no
        // network) to exercise exactly that branch.
        let src = tempfile::tempdir().expect("src tempdir");
        let bin_dir = tempfile::tempdir().expect("bin tempdir");
        let cargo_bin = bin_dir.path().join(bin_name());
        std::fs::write(&cargo_bin, b"prior working binary").unwrap();
        let nonexistent_cargo = bin_dir
            .path()
            .join("definitely-not-a-real-cargo-binary-xyz");

        let result = cargo_install_with_retry_for(
            src.path(),
            false,
            &cargo_bin,
            true,
            nonexistent_cargo.to_str().expect("utf8 tempdir path"),
        );

        assert!(result.is_err(), "spawning a nonexistent program must fail");
        assert!(
            cargo_bin.exists(),
            "the prior binary must be restored even when cargo itself never launched"
        );
        assert_eq!(
            std::fs::read(&cargo_bin).unwrap(),
            b"prior working binary",
            "restore must not corrupt the prior binary's contents"
        );
        let leftovers = sweep_stale_old_exes(&cargo_bin);
        assert!(
            leftovers.is_empty(),
            "restore must not leave a renamed-aside file behind: {leftovers:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("could not launch cargo"),
            "must still name the original launch failure: {msg}"
        );
        assert!(msg.contains("restored"), "{msg}");
    }

    #[test]
    fn install_failure_reports_manual_command_on_launch_failure_when_restore_fails() {
        // Same launch-failure branch as above, but the aside copy is gone
        // by the time restore is attempted, so the fallback manual-command
        // message must fire here too, not only on the build-failure path.
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join(bin_name());
        let old = dir.path().join(old_exe_name(1)); // never created

        let err = install_failure(
            &cargo_bin,
            Some(old.clone()),
            "could not launch cargo (No such file or directory); is it on PATH?".to_string(),
        );

        assert!(!cargo_bin.exists());
        let msg = err.to_string();
        assert!(msg.contains("could not launch cargo"), "{msg}");
        assert!(
            msg.contains("could NOT be restored") || msg.contains("could not be restored"),
            "{msg}"
        );
        assert!(
            msg.contains(&old.display().to_string())
                && msg.contains(&cargo_bin.display().to_string()),
            "{msg}"
        );
        assert!(msg.contains("mv "), "{msg}");
    }

    // --- issue #122: CARGO_INSTALL_ROOT / install.root divergence ----

    #[test]
    fn parse_install_root_toml_reads_the_key() {
        let text = "[install]\nroot = \"/custom/root\"\n";
        assert_eq!(
            parse_install_root_toml(text),
            Some("/custom/root".to_string())
        );
    }

    #[test]
    fn parse_install_root_toml_tolerates_absence_and_garbage() {
        assert_eq!(parse_install_root_toml(""), None);
        assert_eq!(parse_install_root_toml("[build]\njobs = 4\n"), None);
        assert_eq!(parse_install_root_toml("[install]\n"), None);
        assert_eq!(
            parse_install_root_toml("[install]\nroot = 5\n"),
            None,
            "a non-string root must not be misread as a path"
        );
        assert_eq!(parse_install_root_toml("not valid toml {{{"), None);
    }

    #[test]
    fn find_install_root_override_returns_none_with_no_config_anywhere() {
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        assert_eq!(
            find_install_root_override(cwd.path(), cargo_home.path()),
            None
        );
    }

    #[test]
    fn find_install_root_override_finds_cwd_dot_cargo_config() {
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        let cargo_dir = cwd.path().join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "[install]\nroot = \"/repo/local/install\"\n",
        )
        .unwrap();

        let found =
            find_install_root_override(cwd.path(), cargo_home.path()).expect("must be found");
        assert_eq!(found.0, "/repo/local/install");
        assert_eq!(found.1, cargo_dir.display().to_string());
    }

    #[test]
    fn find_install_root_override_searches_ancestors_when_cwd_has_none() {
        let root = tempfile::tempdir().expect("root tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        let parent_cargo_dir = root.path().join(".cargo");
        std::fs::create_dir_all(&parent_cargo_dir).unwrap();
        std::fs::write(
            parent_cargo_dir.join("config.toml"),
            "[install]\nroot = \"/from/ancestor\"\n",
        )
        .unwrap();
        let child = root.path().join("nested").join("repo");
        std::fs::create_dir_all(&child).unwrap();

        let found = find_install_root_override(&child, cargo_home.path()).expect("must be found");
        assert_eq!(found.0, "/from/ancestor");
    }

    #[test]
    fn find_install_root_override_prefers_closest_ancestor() {
        let root = tempfile::tempdir().expect("root tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        let parent_cargo_dir = root.path().join(".cargo");
        std::fs::create_dir_all(&parent_cargo_dir).unwrap();
        std::fs::write(
            parent_cargo_dir.join("config.toml"),
            "[install]\nroot = \"/from/ancestor\"\n",
        )
        .unwrap();
        let child = root.path().join("repo");
        let child_cargo_dir = child.join(".cargo");
        std::fs::create_dir_all(&child_cargo_dir).unwrap();
        std::fs::write(
            child_cargo_dir.join("config.toml"),
            "[install]\nroot = \"/from/repo\"\n",
        )
        .unwrap();

        let found = find_install_root_override(&child, cargo_home.path()).expect("must be found");
        assert_eq!(
            found.0, "/from/repo",
            "the closer config must win, matching cargo's own precedence"
        );
    }

    #[test]
    fn find_install_root_override_falls_back_to_cargo_home() {
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        std::fs::write(
            cargo_home.path().join("config.toml"),
            "[install]\nroot = \"/from/cargo/home\"\n",
        )
        .unwrap();

        let found =
            find_install_root_override(cwd.path(), cargo_home.path()).expect("must be found");
        assert_eq!(found.0, "/from/cargo/home");
        assert_eq!(found.1, cargo_home.path().display().to_string());
    }

    #[test]
    fn find_install_root_override_legacy_config_name_also_read() {
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let cargo_home = tempfile::tempdir().expect("cargo home tempdir");
        let cargo_dir = cwd.path().join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config"),
            "[install]\nroot = \"/legacy/name\"\n",
        )
        .unwrap();

        let found =
            find_install_root_override(cwd.path(), cargo_home.path()).expect("must be found");
        assert_eq!(found.0, "/legacy/name");
    }

    // -- retired sync_md twin cleanup (audit C5 / task 119) ---------------

    #[test]
    fn cleanup_stale_sync_md_twins_removes_recognized_generated_files() {
        let repo = tempfile::tempdir().expect("repo tempdir");
        let rendered_dir = repo.path().join(".memhub").join("rendered");
        std::fs::create_dir_all(&rendered_dir).unwrap();
        std::fs::write(
            rendered_dir.join("AGENTS.md"),
            format!("{STALE_SYNC_MD_MARKER}\n\nsome body\n"),
        )
        .unwrap();
        std::fs::write(
            rendered_dir.join("CLAUDE.md"),
            format!("{STALE_SYNC_MD_MARKER}\n\nother body\n"),
        )
        .unwrap();

        let removed = cleanup_stale_sync_md_twins(repo.path());

        assert_eq!(
            removed.len(),
            2,
            "both twins should be removed: {removed:?}"
        );
        assert!(!rendered_dir.join("AGENTS.md").exists());
        assert!(!rendered_dir.join("CLAUDE.md").exists());
    }

    #[test]
    fn cleanup_stale_sync_md_twins_leaves_unrecognized_content_untouched() {
        // A file at the exact same path that does not carry the sync_md
        // marker (e.g. hand-placed, or written by something else) must
        // never be silently deleted.
        let repo = tempfile::tempdir().expect("repo tempdir");
        let rendered_dir = repo.path().join(".memhub").join("rendered");
        std::fs::create_dir_all(&rendered_dir).unwrap();
        std::fs::write(rendered_dir.join("AGENTS.md"), "not a sync_md twin\n").unwrap();

        let removed = cleanup_stale_sync_md_twins(repo.path());

        assert!(
            removed.is_empty(),
            "unrecognized content must be left alone: {removed:?}"
        );
        assert!(rendered_dir.join("AGENTS.md").exists());
    }

    #[test]
    fn cleanup_stale_sync_md_twins_is_idempotent_when_nothing_to_clean() {
        let repo = tempfile::tempdir().expect("repo tempdir");
        // No `.memhub/rendered/` at all.
        let removed_first = cleanup_stale_sync_md_twins(repo.path());
        assert!(removed_first.is_empty());

        // A second run over the exact same (untouched) repo must also be a
        // clean no-op, never an error.
        let removed_second = cleanup_stale_sync_md_twins(repo.path());
        assert!(removed_second.is_empty());
    }
}

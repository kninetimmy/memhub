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

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// Skip the confirmation prompt before replacing a non-symlink
    /// `~/.local/bin/memhub` shadow.
    pub yes: bool,
}

pub fn run(cwd: &Path, args: UpgradeArgs) -> Result<()> {
    if args.finish {
        finish_phase(cwd, &args)
    } else {
        orchestrate_phase(cwd, &args)
    }
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
    //    rather than half-upgrade.
    println!("==> cargo install --path . --force");
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
    if !status.success() {
        return Err(MemhubError::ExternalCommand {
            command: "cargo install --path . --force".to_string(),
            stderr: "build/install failed; not migrating instances".to_string(),
        });
    }
    println!("    installed -> {}", cargo_bin.display());

    // 2. One-time, order-independent PATH-shadow fix (closes task 39).
    let outcome = fix_path_shadow(&cargo_bin, args.yes)?;
    println!("==> PATH: {}", outcome.message);

    // 3. Re-exec the freshly installed binary for the migrate + verify
    //    pass so migrations run under NEW code. Use the explicit
    //    cargo-bin path, not PATH — PATH may still resolve to a shadow
    //    the user declined to fix.
    let mut child = Command::new(&cargo_bin);
    child
        .arg("upgrade")
        .arg("--finish")
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
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
    let push = |p: &Path, roots: &mut Vec<PathBuf>, seen: &mut std::collections::BTreeSet<String>| {
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

    emit(&reports, pruned, args.json);

    if reports
        .iter()
        .any(|r| matches!(r.status, InstanceStatus::Error(_)))
    {
        std::process::exit(1);
    }
    Ok(())
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
            })
        );
        return Ok(());
    }

    println!("memhub upgrade --dry-run (no changes made)");
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

fn emit(reports: &[InstanceReport], pruned: usize, as_json: bool) {
    let ready = reports
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                InstanceStatus::Ready | InstanceStatus::Migrated
            )
        })
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
// Path helpers
// ---------------------------------------------------------------------

fn bin_name() -> &'static str {
    if cfg!(windows) { "memhub.exe" } else { "memhub" }
}

fn cargo_bin_path() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(h).join("bin").join(bin_name()));
    }
    Ok(db::home_dir()?
        .join(".cargo")
        .join("bin")
        .join(bin_name()))
}

fn local_bin_shadow() -> Result<PathBuf> {
    Ok(db::home_dir()?
        .join(".local")
        .join("bin")
        .join(bin_name()))
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

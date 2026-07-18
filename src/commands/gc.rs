//! `memhub gc` — reclaim disk by deleting superseded build artifacts in
//! this repo's `target/` directory.
//!
//! Cargo's `target/<profile>/deps/` is append-only: every rebuild writes
//! a new hash-suffixed artifact and never deletes the old one. For a
//! crate like memhub that bundles ONNX models via `include_bytes!`, each
//! `libmemhub-<hash>.rlib` is ~1 GB and each integration-test binary is
//! ~1 GB, so a few weeks of `cargo test` accumulates 100+ GB of dead
//! weight Cargo will never reclaim on its own.
//!
//! This GC is deliberately conservative so "keep only the newest build
//! set" can never corrupt a working tree:
//!
//! * It only ever touches **memhub-owned** artifact stems — `memhub`,
//!   `libmemhub`, and any stem matching a top-level `tests/*.rs`,
//!   `benches/*.rs`, or `examples/*.rs` file. Third-party dependency
//!   rlibs (`libserde-*`, `libtokio-*`, …) carry a single hash, never
//!   balloon, and are structurally never considered — *unless*
//!   `[gc] prune_large_thirdparty` is opted in (see below).
//! * For each owned stem it keeps every file of the newest-mtime hash
//!   and deletes the older hashes plus their `.fingerprint/<stem>-<hash>`
//!   dirs. Worst case of deleting a superseded hash is that a stale test
//!   binary is rebuilt on the next `cargo test` — Cargo recovers; no
//!   state is corrupted because the current (newest) set is untouched.
//!
//! Wave 5 (July 2026 improvement review) extended coverage beyond the
//! original `deps/` + `.fingerprint/` sweep:
//!
//! * **U2** — `target/<profile>/build/<stem>-<hash>/` OUT_DIRs (owned
//!   stems only — in practice just `memhub`'s own build script) and
//!   `target/<profile>/examples/` hash-suffixed byproducts are always
//!   swept, no opt-in required.
//! * **U3** — abandoned upgrade-staging shims (`memhub-upgrade-*.exe`)
//!   older than an hour are swept from the OS temp dir on every run,
//!   not just at the start of the next `memhub upgrade`.
//! * **U5(a)** (decision Q12) — superseded `incremental/memhub-*`
//!   session dirs are pruned only when `[gc]
//!   prune_superseded_incremental` is set. Off by default: reversing a
//!   shipped exclusion stays an explicit, narrow opt-in rather than a
//!   unilateral default flip, even though the original "no comparable
//!   disk win" rationale for excluding `incremental/` no longer holds
//!   (measured 14+ GiB of superseded `memhub-*` sessions on one dev
//!   checkout).
//! * **U5(b)** (decision Q12) — non-owned (third-party) `deps/` stems
//!   are only considered when `[gc] prune_large_thirdparty` is set,
//!   and even then only when a stem's largest single-hash footprint
//!   clears `LARGE_THIRDPARTY_THRESHOLD_BYTES` — narrows the opt-in to
//!   genuinely chunky crates (e.g. `ort_sys`, ~294 MB per hash) rather
//!   than every multi-hash dependency.
//! * **U8** (decision Q16) — `.memhub/backups/{rendered,markdown}` is
//!   capped at the 20 newest files each. Report-only unless `[gc]
//!   delete_stale_backups` is set. The legacy one-time
//!   `project.sqlite.k9-bootstrap-backup` is always reported when
//!   present, never auto-deleted. `backups/sync/` is already
//!   single-slot and is left alone entirely.
//!
//! There is deliberately no CLI flag for any Wave 5 opt-in: they are
//! persisted, per-repo policy choices read from `.memhub/config.toml
//! [gc]` (best-effort — a missing/absent/unparsable config leaves every
//! opt-in at its default `false`, so `memhub gc` keeps working exactly
//! as before on a repo that has never run `memhub init`). `dry_run`
//! remains the one long-standing per-invocation choice.
//!
//! Pure `std::fs` — OS-agnostic by construction (macOS + Windows).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::{GcConfig, ProjectConfig};
use crate::{MemhubError, Result};

/// Cargo profiles whose `deps/` accumulate stale hashes.
const PROFILES: &[&str] = &["debug", "release"];

/// A cargo metadata hash is lowercase hex; historically 16 chars but be
/// lenient on the low end so older artifacts still match.
const MIN_HASH_LEN: usize = 8;

/// U5(b) (decision Q12): a non-owned (third-party) `deps/` stem only
/// qualifies for `prune_large_thirdparty` when its largest single-hash
/// footprint clears this bar — narrows the opt-in to genuinely chunky
/// crates (the review's motivating case, `ort_sys`, runs ~294 MB per
/// hash) rather than every multi-hash third-party dependency.
const LARGE_THIRDPARTY_THRESHOLD_BYTES: u64 = 100 * 1024 * 1024;

/// U5(a) (decision Q12): the one `incremental/` stem this sweep prunes
/// when opted in. Scoped to exactly `memhub`, not the full owned-stem
/// set — test/bench incremental caches are KB-scale; memhub's own is
/// the multi-GB hog the July review measured.
const INCREMENTAL_OWNED_STEM: &str = "memhub";

/// U8 (decision Q16): newest-N backup files kept per
/// `.memhub/backups/{rendered,markdown}` directory.
const BACKUPS_RETENTION_COUNT: usize = 20;

/// U8: the one-time legacy DB backup left behind by the K9-to-memhub-
/// primary migration. Always reported when present, never auto-deleted
/// — the user removes it by hand once they've eyeballed it.
const LEGACY_K9_BOOTSTRAP_BACKUP: &str = "project.sqlite.k9-bootstrap-backup";

/// Opt-in gc behaviors (Wave 5). All off by default so `memhub gc`'s
/// output for these classes is byte-identical to pre-Wave-5 memhub
/// until a repo explicitly sets `.memhub/config.toml [gc]` — see the
/// module doc comment. `dry_run` is the one field that is never
/// persisted: always an explicit, per-invocation choice.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcOptions {
    pub dry_run: bool,
    /// U5(a): also prune superseded `incremental/memhub-*` session dirs.
    pub prune_superseded_incremental: bool,
    /// U5(b): also prune large (>= 100 MB/hash) multi-hash third-party
    /// `deps/` artifacts.
    pub prune_large_thirdparty: bool,
    /// U8: actually delete backups beyond the newest 20 (never the
    /// legacy k9 file, which is only ever reported). Off = report-only.
    pub delete_stale_backups: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct GcOutcome {
    pub root: PathBuf,
    pub dry_run: bool,
    pub removed_files: u64,
    pub removed_dirs: u64,
    pub bytes_freed: u64,
    /// Number of stale-artifact groups pruned across `deps/`, `build/`,
    /// `examples/`, `incremental/` (when opted in), and the temp-dir
    /// staging sweep.
    pub groups_pruned: u64,
    /// Human-readable breadcrumbs, one per pruned group plus any
    /// best-effort warnings.
    pub details: Vec<String>,
    /// U8 backups-retention findings. Always populated (possibly
    /// empty/default) independent of `groups_pruned` / `bytes_freed`
    /// above, since it covers a different tree (`.memhub/backups/`,
    /// not `target/`).
    pub backups: BackupsReport,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct BackupsReport {
    pub checked_dirs: Vec<BackupDirOutcome>,
    pub legacy_k9_backup: Option<PathBuf>,
    /// Whether this run operated in delete (not report-only) mode.
    pub deleted: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct BackupDirOutcome {
    pub dir: PathBuf,
    pub kept: usize,
    pub stale_files: usize,
    pub stale_bytes: u64,
}

impl GcOutcome {
    /// One-line summary for the `memhub upgrade` table and the CLI.
    pub fn summary(&self) -> String {
        if self.groups_pruned == 0 {
            return "nothing to reclaim (already at newest build set)".to_string();
        }
        let verb = if self.dry_run { "would free" } else { "freed" };
        format!(
            "{} stale artifact group(s): {} {} ({} files, {} dirs)",
            self.groups_pruned,
            verb,
            human_bytes(self.bytes_freed),
            self.removed_files,
            self.removed_dirs,
        )
    }
}

/// Walk up from `start` to the nearest ancestor containing `Cargo.toml`.
fn find_repo_root(start: &Path) -> Result<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join("Cargo.toml").is_file() {
            return Ok(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    Err(MemhubError::InvalidInput(format!(
        "no Cargo.toml found at or above {}; `memhub gc` only applies to \
         a Rust project's target/ directory",
        start.display()
    )))
}

/// The set of artifact stems memhub owns and is safe to prune.
fn owned_stems(root: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    set.insert("memhub".to_string());
    set.insert("libmemhub".to_string());
    for sub in ["tests", "benches", "examples"] {
        let dir = root.join(sub);
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                set.insert(stem.to_string());
            }
        }
    }
    set
}

/// Split a `deps/` (or `build/`, `examples/`) entry name into `(stem,
/// hash)`. The only `-` in a memhub-owned artifact name is the
/// separator before the cargo hash (stems use `_`), so split on the
/// last `-` and validate the tail is a hex hash. Returns `None` for
/// anything that is not a hash-suffixed artifact (e.g. `.cargo-lock`,
/// an un-hashed final binary).
fn parse_artifact(name: &str) -> Option<(String, String)> {
    let dash = name.rfind('-')?;
    let stem = &name[..dash];
    let rest = &name[dash + 1..];
    let hash = match rest.find('.') {
        Some(dot) => &rest[..dot],
        None => rest,
    };
    if stem.is_empty() || hash.len() < MIN_HASH_LEN || !hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some((stem.to_string(), hash.to_string()))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in rd.flatten() {
        let p = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => total += dir_size(&p),
            _ => {
                if let Ok(md) = fs::symlink_metadata(&p) {
                    total += md.len();
                }
            }
        }
    }
    total
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

struct Artifact {
    path: PathBuf,
    is_dir: bool,
    size: u64,
    mtime: SystemTime,
}

/// Shared keep-newest-per-stem pruning tail: given a fully-populated
/// stem -> hash -> artifacts map, keep each stem's newest-mtime hash
/// group and delete the rest (plus the matching `.fingerprint/<stem>-
/// <hash>` dir when `fingerprint` is given), tallying into `out`. Used
/// by the `deps/`, `build/`, and `examples/` sweeps (U2), which only
/// differ in how `groups` gets populated (ownership/size eligibility).
fn finalize_groups(
    dir_label: &str,
    profile: &str,
    groups: BTreeMap<String, BTreeMap<String, Vec<Artifact>>>,
    fingerprint: Option<&Path>,
    dry_run: bool,
    out: &mut GcOutcome,
) {
    for (stem, by_hash) in groups {
        if by_hash.len() < 2 {
            continue; // only one hash for this stem — nothing superseded
        }
        // Newest hash = the one whose newest file mtime is greatest.
        let keep = by_hash
            .iter()
            .max_by_key(|(_, arts)| {
                arts.iter()
                    .map(|a| a.mtime)
                    .max()
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            })
            .map(|(h, _)| h.clone())
            .unwrap_or_default();

        let mut group_bytes = 0u64;
        let mut group_files = 0u64;
        let mut group_dirs = 0u64;

        for (hash, arts) in &by_hash {
            if *hash == keep {
                continue;
            }
            for a in arts {
                group_bytes += a.size;
                if a.is_dir {
                    group_dirs += 1;
                } else {
                    group_files += 1;
                }
                if !dry_run {
                    let res = if a.is_dir {
                        fs::remove_dir_all(&a.path)
                    } else {
                        fs::remove_file(&a.path)
                    };
                    if let Err(e) = res {
                        out.details
                            .push(format!("warn: could not remove {}: {e}", a.path.display()));
                    }
                }
            }
            // The matching .fingerprint/<stem>-<hash> dir, if present.
            if let Some(fp_root) = fingerprint {
                let fp = fp_root.join(format!("{stem}-{hash}"));
                if fp.is_dir() {
                    group_bytes += dir_size(&fp);
                    group_dirs += 1;
                    if !dry_run && let Err(e) = fs::remove_dir_all(&fp) {
                        out.details
                            .push(format!("warn: could not remove {}: {e}", fp.display()));
                    }
                }
            }
        }

        out.bytes_freed += group_bytes;
        out.removed_files += group_files;
        out.removed_dirs += group_dirs;
        out.groups_pruned += 1;
        out.details.push(format!(
            "{profile}/{dir_label}/{stem}: kept {keep}, {} {} stale hash(es) ({})",
            if dry_run { "would remove" } else { "removed" },
            by_hash.len() - 1,
            human_bytes(group_bytes),
        ));
    }
}

/// Prune one `deps/` + `.fingerprint/` pair for one profile, mutating
/// the running totals on `out`. Owned stems are always in scope;
/// non-owned (third-party) stems are only scanned/considered when
/// `prune_large_thirdparty` is set, and even then only when they clear
/// `LARGE_THIRDPARTY_THRESHOLD_BYTES` (U5b, decision Q12).
fn prune_profile(
    profile: &str,
    deps: &Path,
    fingerprint: &Path,
    owned: &BTreeSet<String>,
    prune_large_thirdparty: bool,
    dry_run: bool,
    out: &mut GcOutcome,
) {
    // stem -> hash -> [artifacts]
    let mut groups: BTreeMap<String, BTreeMap<String, Vec<Artifact>>> = BTreeMap::new();

    let Ok(rd) = fs::read_dir(deps) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((stem, hash)) = parse_artifact(name) else {
            continue;
        };
        // Third-party stems are only worth scanning at all when the
        // opt-in is set — keeps the default scan identical to today.
        if !owned.contains(&stem) && !prune_large_thirdparty {
            continue;
        }
        let md = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = md.is_dir();
        let size = if is_dir { dir_size(&path) } else { md.len() };
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        groups
            .entry(stem)
            .or_default()
            .entry(hash)
            .or_default()
            .push(Artifact {
                path,
                is_dir,
                size,
                mtime,
            });
    }

    if prune_large_thirdparty {
        // Non-owned stems additionally need their largest hash-group to
        // clear the size bar — narrows the opt-in to genuinely chunky
        // crates rather than every multi-hash third-party dependency.
        groups.retain(|stem, by_hash| {
            owned.contains(stem) || {
                let max_group_bytes = by_hash
                    .values()
                    .map(|arts| arts.iter().map(|a| a.size).sum::<u64>())
                    .max()
                    .unwrap_or(0);
                max_group_bytes >= LARGE_THIRDPARTY_THRESHOLD_BYTES
            }
        });
    }

    finalize_groups("deps", profile, groups, Some(fingerprint), dry_run, out);
}

/// U2: sweep `target/<profile>/build/<stem>-<hash>/` OUT_DIRs for owned
/// stems (in practice just `memhub` — the only crate here with its own
/// `build.rs`) using the identical keep-newest-per-stem rule as
/// `deps/`. Cargo hashes a build script's OUT_DIR with the same hash it
/// uses for the matching `.fingerprint/<stem>-<hash>` entry, so that
/// fingerprint dir is removed alongside it too, exactly like `deps/`.
/// Third-party build dirs (e.g. `ort-sys-<hash>/`) are never touched —
/// there is no opt-in escape hatch here, unlike `deps/`'s U5(b); the
/// review's motivating third-party bloat was specifically `deps/`
/// rlibs, not build OUT_DIRs.
fn prune_build(
    profile: &str,
    build: &Path,
    fingerprint: &Path,
    owned: &BTreeSet<String>,
    dry_run: bool,
    out: &mut GcOutcome,
) {
    let mut groups: BTreeMap<String, BTreeMap<String, Vec<Artifact>>> = BTreeMap::new();

    let Ok(rd) = fs::read_dir(build) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((stem, hash)) = parse_artifact(name) else {
            continue;
        };
        if !owned.contains(&stem) {
            continue;
        }
        let md = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !md.is_dir() {
            continue; // build/ entries of interest are always OUT_DIRs
        }
        let size = dir_size(&path);
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        groups
            .entry(stem)
            .or_default()
            .entry(hash)
            .or_default()
            .push(Artifact {
                path,
                is_dir: true,
                size,
                mtime,
            });
    }

    finalize_groups("build", profile, groups, Some(fingerprint), dry_run, out);
}

/// U2: sweep `target/<profile>/examples/` hash-suffixed byproducts
/// (`<stem>-<hash>.d`, `lib<stem>-<hash>.rmeta`, …) using the same
/// keep-newest-per-stem rule. No ownership filter is needed — unlike
/// `deps/`, Cargo only ever writes *this* crate's own example
/// artifacts into `examples/`, never a dependency's. The final,
/// un-hashed `<stem>.exe` / `.pdb` / `.lib` / `.exp` that cargo
/// overwrites in place on every rebuild is left alone: it has no hash
/// suffix, so `parse_artifact` already skips it.
fn prune_examples(profile: &str, examples: &Path, dry_run: bool, out: &mut GcOutcome) {
    let mut groups: BTreeMap<String, BTreeMap<String, Vec<Artifact>>> = BTreeMap::new();

    let Ok(rd) = fs::read_dir(examples) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((stem, hash)) = parse_artifact(name) else {
            continue;
        };
        let md = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = md.is_dir();
        let size = if is_dir { dir_size(&path) } else { md.len() };
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        groups
            .entry(stem)
            .or_default()
            .entry(hash)
            .or_default()
            .push(Artifact {
                path,
                is_dir,
                size,
                mtime,
            });
    }

    finalize_groups("examples", profile, groups, None, dry_run, out);
}

/// U5(a) (decision Q12), opt-in: prune superseded
/// `incremental/memhub-<session>` dirs. Cargo's incremental-cache
/// naming departs from the hex-hash convention used elsewhere in
/// `target/` — the suffix is a lowercase, not-necessarily-hex session
/// id — so this does not reuse `parse_artifact`; it only needs the
/// stem/suffix split on the last `-`, the same invariant relied on
/// throughout this file (memhub stems use `_`, never `-`). Keeps the
/// newest-mtime dir for the `memhub` stem specifically, not the full
/// owned-stem set: test/bench incremental caches are KB-scale, while
/// `memhub`'s own routinely reaches multiple GB per generation (the
/// July review measured 14+ GiB total across superseded generations).
fn prune_incremental(profile: &str, incremental: &Path, dry_run: bool, out: &mut GcOutcome) {
    let mut sessions: Vec<Artifact> = Vec::new();

    let Ok(rd) = fs::read_dir(incremental) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(dash) = name.rfind('-') else {
            continue;
        };
        if &name[..dash] != INCREMENTAL_OWNED_STEM {
            continue;
        }
        let md = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !md.is_dir() {
            continue;
        }
        let size = dir_size(&path);
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        sessions.push(Artifact {
            path,
            is_dir: true,
            size,
            mtime,
        });
    }

    if sessions.len() < 2 {
        return; // zero or one generation present — nothing superseded
    }

    let keep_idx = sessions
        .iter()
        .enumerate()
        .max_by_key(|(_, a)| a.mtime)
        .map(|(i, _)| i)
        .unwrap_or(0);

    let mut group_bytes = 0u64;
    let mut group_dirs = 0u64;
    for (i, a) in sessions.iter().enumerate() {
        if i == keep_idx {
            continue;
        }
        group_bytes += a.size;
        group_dirs += 1;
        if !dry_run && let Err(e) = fs::remove_dir_all(&a.path) {
            out.details
                .push(format!("warn: could not remove {}: {e}", a.path.display()));
        }
    }

    out.bytes_freed += group_bytes;
    out.removed_dirs += group_dirs;
    out.groups_pruned += 1;
    out.details.push(format!(
        "{profile}/incremental/{INCREMENTAL_OWNED_STEM}: kept newest session, {} {} superseded session(s) ({})",
        if dry_run { "would remove" } else { "removed" },
        sessions.len() - 1,
        human_bytes(group_bytes),
    ));
}

/// U3: sweep abandoned upgrade-staging shims (`memhub-upgrade-*.exe`)
/// older than an hour from `temp_dir` — the ~272 MB inter-upgrade shim
/// leak the July review measured (previously swept only as a side
/// effect of the *next* `memhub upgrade` run, via
/// `upgrade::sweep_stale_staging`). Kept as an independent copy here
/// rather than a shared call into `upgrade.rs`, which is owned by the
/// sibling Wave 5 issue (#89) and out of scope for this change; this
/// copy also threads through `dry_run` (report, don't delete), which
/// the upgrade-time original — a fire-and-forget pre-flight sweep, not
/// a user-facing report — has no need for. `temp_dir` is an explicit
/// parameter (rather than reading `std::env::temp_dir()` internally)
/// so tests exercise this against a fixture directory instead of the
/// real, process-global OS temp dir.
fn sweep_stale_staging(temp_dir: &Path, dry_run: bool, out: &mut GcOutcome) {
    let Ok(rd) = fs::read_dir(temp_dir) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(3600);

    let mut count = 0u64;
    let mut bytes = 0u64;
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("memhub-upgrade-") && name.ends_with(".exe")) {
            continue;
        }
        let Ok(md) = entry.metadata() else {
            continue;
        };
        let old = md.modified().map(|m| m < cutoff).unwrap_or(false);
        if !old {
            continue;
        }
        count += 1;
        bytes += md.len();
        if !dry_run && let Err(e) = fs::remove_file(entry.path()) {
            out.details.push(format!(
                "warn: could not remove {}: {e}",
                entry.path().display()
            ));
        }
    }

    if count > 0 {
        out.bytes_freed += bytes;
        out.removed_files += count;
        out.groups_pruned += 1;
        out.details.push(format!(
            "staging: {} {} abandoned upgrade shim(s) ({})",
            if dry_run { "would remove" } else { "removed" },
            count,
            human_bytes(bytes),
        ));
    }
}

/// U8 (decision Q16): `.memhub/backups/{rendered,markdown}` retention.
/// Report-only unless `delete` (and never when `dry_run`, which always
/// wins); `backups/sync/` is already single-slot (one fixed
/// `last-replaced.sqlite`, never accumulates) and is deliberately left
/// alone. Also flags — but never deletes — the legacy one-time
/// `project.sqlite.k9-bootstrap-backup` left behind by the
/// K9-to-memhub-primary migration, if still present.
fn backups_retention(root: &Path, dry_run: bool, delete: bool, out: &mut GcOutcome) {
    let memhub_dir = root.join(".memhub");
    let backups_dir = memhub_dir.join("backups");
    let will_delete = delete && !dry_run;

    for subdir in ["rendered", "markdown"] {
        let dir = backups_dir.join(subdir);
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };

        let mut files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(md) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !md.is_file() {
                continue;
            }
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((path, mtime, md.len()));
        }

        if files.len() <= BACKUPS_RETENTION_COUNT {
            continue; // within retention — nothing to report or prune
        }

        // Newest first; the tail beyond the retention count is stale.
        files.sort_by_key(|f| std::cmp::Reverse(f.1));
        let stale = &files[BACKUPS_RETENTION_COUNT..];
        let stale_bytes: u64 = stale.iter().map(|(_, _, len)| len).sum();

        if will_delete {
            for (path, _, _) in stale {
                if let Err(e) = fs::remove_file(path) {
                    out.details
                        .push(format!("warn: could not remove {}: {e}", path.display()));
                }
            }
            // Faithful reporting: only REAL deletions count toward the
            // headline totals `summary()` reads — mirrors
            // `finalize_groups`/`sweep_stale_staging`, which roll their
            // deletions into these same counters. Report-only mode
            // (the `else` below) never reaches this branch, so it can
            // never inflate `bytes_freed`/`removed_files`.
            out.bytes_freed += stale_bytes;
            out.removed_files += stale.len() as u64;
            out.groups_pruned += 1;
        }

        out.backups.checked_dirs.push(BackupDirOutcome {
            dir: dir.clone(),
            kept: BACKUPS_RETENTION_COUNT,
            stale_files: stale.len(),
            stale_bytes,
        });
        out.details.push(if will_delete {
            format!(
                "backups/{subdir}: removed {} stale file(s) beyond the newest {} ({})",
                stale.len(),
                BACKUPS_RETENTION_COUNT,
                human_bytes(stale_bytes),
            )
        } else {
            format!(
                "backups/{subdir}: {} stale file(s) beyond the newest {} would be removed ({}); \
                 set [gc] delete_stale_backups = true to delete",
                stale.len(),
                BACKUPS_RETENTION_COUNT,
                human_bytes(stale_bytes),
            )
        });
    }

    let legacy = memhub_dir.join(LEGACY_K9_BOOTSTRAP_BACKUP);
    if legacy.is_file() {
        out.details.push(format!(
            "backups: legacy {LEGACY_K9_BOOTSTRAP_BACKUP} present — safe to remove by hand \
             (never auto-deleted)"
        ));
        out.backups.legacy_k9_backup = Some(legacy);
    }

    out.backups.deleted = will_delete;
}

/// Best-effort, side-effect-free read of this repo's persisted `[gc]`
/// opt-ins. Unlike `db::open_project`, this never requires (or
/// creates) an initialized memhub project — `memhub gc` has always
/// worked on any Rust repo with a `Cargo.toml`, memhub-initialized or
/// not, and must keep doing so. A missing `.memhub/` or a missing
/// `config.toml` is the normal, silent case: every opt-in resolves to
/// `GcConfig::default()` (all off), so an untouched or non-memhub repo
/// behaves exactly as before Wave 5. A `config.toml` that *exists* but
/// fails to parse also falls back to all-off — never a hard error,
/// since one malformed config must not break `gc` — but that case gets
/// a breadcrumb in the returned `Option<String>` (fail-loud nit: a
/// malformed config silently disabling an intended
/// `delete_stale_backups = true` should say so, not go quiet).
fn load_gc_config_best_effort(root: &Path) -> (GcConfig, Option<String>) {
    let config_path = root.join(".memhub").join("config.toml");
    if !config_path.is_file() {
        return (GcConfig::default(), None);
    }
    match ProjectConfig::load(&config_path) {
        Ok(c) => (c.gc, None),
        Err(e) => (
            GcConfig::default(),
            Some(format!(
                "warn: {} did not parse ({e}); all [gc] opt-ins default to off this run",
                config_path.display()
            )),
        ),
    }
}

/// Reclaim superseded build artifacts under `<repo>/target`. When
/// `dry_run` is set nothing is deleted; the outcome reports what would
/// be freed. Also picks up any persisted `.memhub/config.toml [gc]`
/// opt-ins (U5/U8) — see `GcOptions` and the module doc comment.
pub fn run(start: &Path, dry_run: bool) -> Result<GcOutcome> {
    run_with_options(
        start,
        GcOptions {
            dry_run,
            ..GcOptions::default()
        },
    )
}

/// Same as `run`, with the full opt-in set explicit rather than just
/// `dry_run`. Persisted `.memhub/config.toml [gc]` settings are ORed
/// in on top of whatever `options` already requests, so a repo that
/// has opted in via config gets that behavior from every call site
/// (direct `memhub gc`, and the auto-gc inside `memhub upgrade`)
/// without either caller needing to re-read config itself.
pub fn run_with_options(start: &Path, options: GcOptions) -> Result<GcOutcome> {
    run_with_options_in_temp(start, options, &std::env::temp_dir())
}

/// Same as `run_with_options`, with the staging-sweep temp directory
/// (U3) parameterized out for tests, which substitute a fixture dir so
/// they never touch the real, process-global `%TEMP%`/`$TMPDIR` —
/// shared with any real in-flight `memhub upgrade` on the machine.
fn run_with_options_in_temp(
    start: &Path,
    mut options: GcOptions,
    temp_dir: &Path,
) -> Result<GcOutcome> {
    let root = find_repo_root(start)?;

    let (gc_config, gc_config_warning) = load_gc_config_best_effort(&root);
    options.prune_superseded_incremental |= gc_config.prune_superseded_incremental;
    options.prune_large_thirdparty |= gc_config.prune_large_thirdparty;
    options.delete_stale_backups |= gc_config.delete_stale_backups;

    let owned = owned_stems(&root);
    let mut out = GcOutcome {
        root: root.clone(),
        dry_run: options.dry_run,
        removed_files: 0,
        removed_dirs: 0,
        bytes_freed: 0,
        groups_pruned: 0,
        details: Vec::new(),
        backups: BackupsReport::default(),
    };
    if let Some(warning) = gc_config_warning {
        out.details.push(warning);
    }

    let target = root.join("target");
    for profile in PROFILES {
        let profile_dir = target.join(profile);
        let fingerprint = profile_dir.join(".fingerprint");

        let deps = profile_dir.join("deps");
        if deps.is_dir() {
            prune_profile(
                profile,
                &deps,
                &fingerprint,
                &owned,
                options.prune_large_thirdparty,
                options.dry_run,
                &mut out,
            );
        }

        // U2: build/<stem>-<hash> OUT_DIRs — always on, owned stems only.
        let build = profile_dir.join("build");
        if build.is_dir() {
            prune_build(
                profile,
                &build,
                &fingerprint,
                &owned,
                options.dry_run,
                &mut out,
            );
        }

        // U2: examples/ hash-suffixed byproducts — always on.
        let examples = profile_dir.join("examples");
        if examples.is_dir() {
            prune_examples(profile, &examples, options.dry_run, &mut out);
        }

        // U5(a): superseded incremental/memhub-* sessions — opt-in.
        if options.prune_superseded_incremental {
            let incremental = profile_dir.join("incremental");
            if incremental.is_dir() {
                prune_incremental(profile, &incremental, options.dry_run, &mut out);
            }
        }
    }

    // U3: sweep abandoned upgrade-staging shims — always on.
    sweep_stale_staging(temp_dir, options.dry_run, &mut out);

    // U8: backups retention — report-only unless delete_stale_backups.
    backups_retention(
        &root,
        options.dry_run,
        options.delete_stale_backups,
        &mut out,
    );

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::thread::sleep;

    fn touch(path: &Path, bytes: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
    }

    /// Backdate (or otherwise set) a file's mtime without needing real
    /// wall-clock delay — used wherever a test needs files to be
    /// clearly ordered or clearly past a fixed cutoff (e.g. the
    /// staging sweep's 1-hour bar) without sleeping for real.
    fn set_mtime(path: &Path, when: SystemTime) {
        let f = File::options().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    fn scaffold() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        File::create(root.join("Cargo.toml")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        File::create(root.join("tests/foundation.rs")).unwrap();
        td
    }

    /// Test-only: same as `run`, but hermetic — routes the U3 staging
    /// sweep through a scratch fixture dir instead of the real,
    /// process-global OS temp dir. `run`/`run_with_options` always use
    /// the real temp dir (correct in production), so any test that
    /// doesn't specifically exercise the staging sweep must go through
    /// this instead, or it both pollutes and is at the mercy of
    /// whatever `memhub-upgrade-*.exe` shims happen to be sitting in
    /// `%TEMP%`/`$TMPDIR` on the machine running the test.
    fn run_hermetic(start: &Path, dry_run: bool) -> Result<GcOutcome> {
        run_with_options_hermetic(
            start,
            GcOptions {
                dry_run,
                ..GcOptions::default()
            },
        )
    }

    /// Same as `run_with_options`, hermetic — see `run_hermetic`.
    fn run_with_options_hermetic(start: &Path, options: GcOptions) -> Result<GcOutcome> {
        let scratch_temp = tempfile::tempdir().unwrap();
        run_with_options_in_temp(start, options, scratch_temp.path())
    }

    #[test]
    fn parse_artifact_accepts_hash_and_rejects_junk() {
        assert_eq!(
            parse_artifact("libmemhub-9596fa5f217857ee.rlib"),
            Some(("libmemhub".to_string(), "9596fa5f217857ee".to_string()))
        );
        // bare test binary, no extension
        assert_eq!(
            parse_artifact("foundation-1f3e35d67f85104d"),
            Some(("foundation".to_string(), "1f3e35d67f85104d".to_string()))
        );
        assert_eq!(parse_artifact(".cargo-lock"), None);
        assert_eq!(parse_artifact("memhub"), None);
        assert_eq!(parse_artifact("libserde-xyz.rlib"), None); // not hex
    }

    #[test]
    fn keeps_newest_hash_and_prunes_older_for_owned_stems() {
        let td = scaffold();
        let root = td.path();
        let deps = root.join("target/debug/deps");
        let fp = root.join("target/debug/.fingerprint");

        // Two libmemhub hashes; the "new" one is written second so its
        // mtime is later.
        touch(&deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib"), 1000);
        touch(&deps.join("libmemhub-aaaaaaaaaaaaaaaa.rmeta"), 500);
        fs::create_dir_all(fp.join("libmemhub-aaaaaaaaaaaaaaaa")).unwrap();
        touch(&fp.join("libmemhub-aaaaaaaaaaaaaaaa/dep-lib"), 10);
        // A third-party dep with a single hash — must never be touched.
        touch(&deps.join("libserde-cccccccccccccccc.rlib"), 2000);

        sleep(Duration::from_millis(20));
        touch(&deps.join("libmemhub-bbbbbbbbbbbbbbbb.rlib"), 1000);

        let out = run_hermetic(root, false).unwrap();

        assert!(deps.join("libmemhub-bbbbbbbbbbbbbbbb.rlib").exists());
        assert!(!deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib").exists());
        assert!(!deps.join("libmemhub-aaaaaaaaaaaaaaaa.rmeta").exists());
        assert!(!fp.join("libmemhub-aaaaaaaaaaaaaaaa").exists());
        // third-party untouched
        assert!(deps.join("libserde-cccccccccccccccc.rlib").exists());
        assert_eq!(out.groups_pruned, 1);
        assert!(out.removed_files >= 2);
        assert!(out.bytes_freed >= 1500);
    }

    #[test]
    fn dry_run_changes_nothing() {
        let td = scaffold();
        let root = td.path();
        let deps = root.join("target/debug/deps");
        touch(&deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib"), 1000);
        sleep(Duration::from_millis(20));
        touch(&deps.join("libmemhub-bbbbbbbbbbbbbbbb.rlib"), 1000);

        let out = run_hermetic(root, true).unwrap();

        assert!(deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib").exists());
        assert!(deps.join("libmemhub-bbbbbbbbbbbbbbbb.rlib").exists());
        assert_eq!(out.groups_pruned, 1);
        assert!(out.bytes_freed >= 1000);
    }

    #[test]
    fn single_hash_is_noop() {
        let td = scaffold();
        let root = td.path();
        let deps = root.join("target/debug/deps");
        touch(&deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib"), 1000);

        let out = run_hermetic(root, false).unwrap();
        assert_eq!(out.groups_pruned, 0);
        assert!(deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib").exists());
    }

    #[test]
    fn no_cargo_toml_is_clear_error() {
        let td = tempfile::tempdir().unwrap();
        let err = run(td.path(), false).unwrap_err();
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    // -- U2: build/ OUT_DIRs ------------------------------------------------

    #[test]
    fn build_out_dirs_keep_newest_and_prune_older() {
        let td = scaffold();
        let root = td.path();
        let build = root.join("target/debug/build");
        let fp = root.join("target/debug/.fingerprint");

        touch(&build.join("memhub-aaaaaaaaaaaaaaaa/out/model.onnx"), 1000);
        fs::create_dir_all(fp.join("memhub-aaaaaaaaaaaaaaaa")).unwrap();
        touch(&fp.join("memhub-aaaaaaaaaaaaaaaa/output"), 10);
        // A third-party build script OUT_DIR — never touched; there is
        // no opt-in for build/, unlike deps/'s U5(b).
        touch(&build.join("ort-sys-dddddddddddddddd/out/marker"), 2000);

        sleep(Duration::from_millis(20));
        touch(&build.join("memhub-bbbbbbbbbbbbbbbb/out/model.onnx"), 1000);

        let out = run_hermetic(root, false).unwrap();

        assert!(build.join("memhub-bbbbbbbbbbbbbbbb").exists());
        assert!(!build.join("memhub-aaaaaaaaaaaaaaaa").exists());
        assert!(!fp.join("memhub-aaaaaaaaaaaaaaaa").exists());
        assert!(build.join("ort-sys-dddddddddddddddd").exists());
        assert_eq!(out.groups_pruned, 1);
        assert!(out.bytes_freed >= 1000);
    }

    // -- U2: examples/ hash-suffixed byproducts ------------------------------

    #[test]
    fn examples_artifacts_keep_newest_prune_older_leave_current_binary_untouched() {
        let td = scaffold();
        let root = td.path();
        let examples = root.join("target/debug/examples");

        touch(
            &examples.join("librerank_bakeoff-aaaaaaaaaaaaaaaa.rmeta"),
            100,
        );
        touch(&examples.join("rerank_bakeoff-aaaaaaaaaaaaaaaa.d"), 50);
        sleep(Duration::from_millis(20));
        touch(
            &examples.join("librerank_bakeoff-bbbbbbbbbbbbbbbb.rmeta"),
            100,
        );
        touch(&examples.join("rerank_bakeoff-bbbbbbbbbbbbbbbb.d"), 50);
        // The un-hashed "current" copy cargo overwrites in place —
        // must never be treated as a stale artifact.
        touch(&examples.join("rerank_bakeoff.exe"), 9000);

        let out = run_hermetic(root, false).unwrap();

        assert!(
            examples
                .join("librerank_bakeoff-bbbbbbbbbbbbbbbb.rmeta")
                .exists()
        );
        assert!(examples.join("rerank_bakeoff-bbbbbbbbbbbbbbbb.d").exists());
        assert!(
            !examples
                .join("librerank_bakeoff-aaaaaaaaaaaaaaaa.rmeta")
                .exists()
        );
        assert!(!examples.join("rerank_bakeoff-aaaaaaaaaaaaaaaa.d").exists());
        assert!(examples.join("rerank_bakeoff.exe").exists());
        assert_eq!(out.groups_pruned, 2); // librerank_bakeoff + rerank_bakeoff stems
    }

    // -- U3: staging shim sweep ----------------------------------------------

    #[test]
    fn staging_sweep_removes_old_shims_and_keeps_fresh_ones() {
        let td = scaffold();
        let root = td.path();
        let temp = tempfile::tempdir().unwrap();

        let old_shim = temp.path().join("memhub-upgrade-1111-old.exe");
        touch(&old_shim, 500);
        set_mtime(&old_shim, SystemTime::now() - Duration::from_secs(7200));

        let fresh_shim = temp.path().join("memhub-upgrade-2222-fresh.exe");
        touch(&fresh_shim, 500);

        // A decoy that must survive regardless of age — not a staging shim.
        let unrelated = temp.path().join("some-other-file.exe");
        touch(&unrelated, 500);
        set_mtime(&unrelated, SystemTime::now() - Duration::from_secs(7200));

        let out = run_with_options_in_temp(root, GcOptions::default(), temp.path()).unwrap();

        assert!(!old_shim.exists(), "old shim should be swept");
        assert!(fresh_shim.exists(), "fresh shim should survive");
        assert!(unrelated.exists(), "non-shim file must never be touched");
        assert!(out.details.iter().any(|d| d.contains("staging")));
    }

    #[test]
    fn staging_sweep_dry_run_reports_without_removing() {
        let td = scaffold();
        let root = td.path();
        let temp = tempfile::tempdir().unwrap();

        let old_shim = temp.path().join("memhub-upgrade-3333-old.exe");
        touch(&old_shim, 500);
        set_mtime(&old_shim, SystemTime::now() - Duration::from_secs(7200));

        let out = run_with_options_in_temp(
            root,
            GcOptions {
                dry_run: true,
                ..GcOptions::default()
            },
            temp.path(),
        )
        .unwrap();

        assert!(old_shim.exists(), "dry-run must not delete");
        assert!(out.details.iter().any(|d| d.contains("would remove")));
    }

    // -- U5(a): superseded incremental/memhub-* sessions, opt-in -------------

    #[test]
    fn incremental_sessions_untouched_by_default_then_pruned_when_opted_in() {
        let td = scaffold();
        let root = td.path();
        let incremental = root.join("target/debug/incremental");

        touch(&incremental.join("memhub-oldsession1/marker"), 1000);
        sleep(Duration::from_millis(20));
        touch(&incremental.join("memhub-newsession2/marker"), 1000);
        // A test-target incremental dir — out of scope; U5(a) is
        // scoped to the memhub stem specifically.
        touch(&incremental.join("foundation-abc123/marker"), 10);

        let scratch_temp = tempfile::tempdir().unwrap();

        // Default: both memhub sessions survive untouched.
        let out =
            run_with_options_in_temp(root, GcOptions::default(), scratch_temp.path()).unwrap();
        assert!(incremental.join("memhub-oldsession1").exists());
        assert!(incremental.join("memhub-newsession2").exists());
        assert_eq!(out.groups_pruned, 0);

        // Opted in: only the older session is pruned.
        let out = run_with_options_in_temp(
            root,
            GcOptions {
                prune_superseded_incremental: true,
                ..GcOptions::default()
            },
            scratch_temp.path(),
        )
        .unwrap();
        assert!(!incremental.join("memhub-oldsession1").exists());
        assert!(incremental.join("memhub-newsession2").exists());
        assert!(incremental.join("foundation-abc123").exists());
        assert_eq!(out.groups_pruned, 1);
    }

    #[test]
    fn incremental_opt_in_via_persisted_config() {
        let td = scaffold();
        let root = td.path();
        let incremental = root.join("target/debug/incremental");

        touch(&incremental.join("memhub-oldsession1/marker"), 1000);
        sleep(Duration::from_millis(20));
        touch(&incremental.join("memhub-newsession2/marker"), 1000);

        let memhub_dir = root.join(".memhub");
        fs::create_dir_all(&memhub_dir).unwrap();
        fs::write(
            memhub_dir.join("config.toml"),
            r#"project_name = "test"
auto_sync_md = false
log_level = "info"

[gc]
prune_superseded_incremental = true
"#,
        )
        .unwrap();

        // Plain `run` (no explicit GcOptions) must still honor the
        // persisted config — this is the only way U5/U8 opt in, since
        // gc exposes no CLI flags for them. `run_hermetic` applies the
        // identical default-options + config-merge path as `run`
        // itself, just with an isolated staging temp dir.
        let out = run_hermetic(root, false).unwrap();

        assert!(!incremental.join("memhub-oldsession1").exists());
        assert!(incremental.join("memhub-newsession2").exists());
        assert_eq!(out.groups_pruned, 1);
    }

    // -- U5(b): large multi-hash third-party deps/, opt-in + size-gated -----

    #[test]
    fn thirdparty_stem_untouched_by_default_and_when_under_threshold() {
        let td = scaffold();
        let root = td.path();
        let deps = root.join("target/debug/deps");

        touch(&deps.join("libort_sys-aaaaaaaaaaaaaaaa.rlib"), 1000);
        sleep(Duration::from_millis(20));
        touch(&deps.join("libort_sys-bbbbbbbbbbbbbbbb.rlib"), 1000);

        // Default: untouched.
        let out = run_hermetic(root, false).unwrap();
        assert!(deps.join("libort_sys-aaaaaaaaaaaaaaaa.rlib").exists());
        assert!(deps.join("libort_sys-bbbbbbbbbbbbbbbb.rlib").exists());
        assert_eq!(out.groups_pruned, 0);

        // Opted in, but well under the size bar: still untouched.
        let out = run_with_options_hermetic(
            root,
            GcOptions {
                prune_large_thirdparty: true,
                ..GcOptions::default()
            },
        )
        .unwrap();
        assert!(deps.join("libort_sys-aaaaaaaaaaaaaaaa.rlib").exists());
        assert!(deps.join("libort_sys-bbbbbbbbbbbbbbbb.rlib").exists());
        assert_eq!(out.groups_pruned, 0);
    }

    #[test]
    fn thirdparty_stem_pruned_when_opted_in_and_over_threshold() {
        let td = scaffold();
        let root = td.path();
        let deps = root.join("target/debug/deps");

        touch(
            &deps.join("libort_sys-aaaaaaaaaaaaaaaa.rlib"),
            (LARGE_THIRDPARTY_THRESHOLD_BYTES + 1024) as usize,
        );
        sleep(Duration::from_millis(20));
        touch(&deps.join("libort_sys-bbbbbbbbbbbbbbbb.rlib"), 1000);

        let out = run_with_options_hermetic(
            root,
            GcOptions {
                prune_large_thirdparty: true,
                ..GcOptions::default()
            },
        )
        .unwrap();

        assert!(deps.join("libort_sys-bbbbbbbbbbbbbbbb.rlib").exists());
        assert!(!deps.join("libort_sys-aaaaaaaaaaaaaaaa.rlib").exists());
        assert_eq!(out.groups_pruned, 1);
        assert!(out.bytes_freed >= LARGE_THIRDPARTY_THRESHOLD_BYTES);
    }

    // -- U8: backups retention -----------------------------------------------

    #[test]
    fn backups_retention_report_only_by_default_keeps_all_files() {
        let td = scaffold();
        let root = td.path();
        let rendered = root.join(".memhub/backups/rendered");
        fs::create_dir_all(&rendered).unwrap();

        for i in 0..25u64 {
            let p = rendered.join(format!("backup-{i}.bak"));
            touch(&p, 10);
            set_mtime(&p, SystemTime::now() - Duration::from_secs(i));
        }

        let out = run_hermetic(root, false).unwrap();

        assert_eq!(
            fs::read_dir(&rendered).unwrap().count(),
            25,
            "report-only must not delete"
        );
        assert_eq!(out.backups.checked_dirs.len(), 1);
        assert_eq!(out.backups.checked_dirs[0].stale_files, 5);
        assert!(!out.backups.deleted);
        assert!(out.details.iter().any(|d| d.contains("would be removed")));
        // Report-only must NOT inflate the headline totals `summary()`
        // reads — only a real deletion counts.
        assert_eq!(out.groups_pruned, 0);
        assert_eq!(out.bytes_freed, 0);
        assert_eq!(out.removed_files, 0);
        assert_eq!(
            out.summary(),
            "nothing to reclaim (already at newest build set)"
        );
    }

    #[test]
    fn backups_retention_deletes_beyond_newest_20_when_opted_in() {
        let td = scaffold();
        let root = td.path();
        let rendered = root.join(".memhub/backups/rendered");
        fs::create_dir_all(&rendered).unwrap();

        for i in 0..25u64 {
            let p = rendered.join(format!("backup-{i}.bak"));
            touch(&p, 10);
            set_mtime(&p, SystemTime::now() - Duration::from_secs(i));
        }

        let out = run_with_options_hermetic(
            root,
            GcOptions {
                delete_stale_backups: true,
                ..GcOptions::default()
            },
        )
        .unwrap();

        assert_eq!(fs::read_dir(&rendered).unwrap().count(), 20);
        assert!(out.backups.deleted);
        // The 5 oldest (largest `i`, per the mtime offset above) are gone.
        for i in 20..25u64 {
            assert!(!rendered.join(format!("backup-{i}.bak")).exists());
        }
        for i in 0..20u64 {
            assert!(rendered.join(format!("backup-{i}.bak")).exists());
        }
        // A real deletion must be faithfully reflected in the headline
        // totals `summary()` reads (matches `sweep_stale_staging` /
        // `finalize_groups`) — otherwise `memhub gc` prints "nothing to
        // reclaim" / "freed 0 B" after actually freeing space.
        assert_eq!(out.groups_pruned, 1);
        assert_eq!(out.removed_files, 5);
        assert_eq!(out.bytes_freed, 50); // 5 files * 10 bytes each
        let summary = out.summary();
        assert_ne!(summary, "nothing to reclaim (already at newest build set)");
        assert!(summary.contains("freed"), "summary: {summary}");
    }

    #[test]
    fn backups_retention_dry_run_overrides_delete_flag() {
        let td = scaffold();
        let root = td.path();
        let rendered = root.join(".memhub/backups/rendered");
        fs::create_dir_all(&rendered).unwrap();

        for i in 0..25u64 {
            let p = rendered.join(format!("backup-{i}.bak"));
            touch(&p, 10);
            set_mtime(&p, SystemTime::now() - Duration::from_secs(i));
        }

        let out = run_with_options_hermetic(
            root,
            GcOptions {
                dry_run: true,
                delete_stale_backups: true,
                ..GcOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_dir(&rendered).unwrap().count(),
            25,
            "dry-run must override the delete flag"
        );
        assert!(!out.backups.deleted);
        // dry_run must win over delete_stale_backups for the headline
        // totals too, not just for the files on disk.
        assert_eq!(out.groups_pruned, 0);
        assert_eq!(out.bytes_freed, 0);
        assert_eq!(out.removed_files, 0);
        assert_eq!(
            out.summary(),
            "nothing to reclaim (already at newest build set)"
        );
    }

    #[test]
    fn backups_retention_reports_legacy_k9_backup_without_deleting() {
        let td = scaffold();
        let root = td.path();
        let memhub_dir = root.join(".memhub");
        fs::create_dir_all(&memhub_dir).unwrap();
        let legacy = memhub_dir.join("project.sqlite.k9-bootstrap-backup");
        touch(&legacy, 100);

        let out = run_with_options_hermetic(
            root,
            GcOptions {
                delete_stale_backups: true,
                ..GcOptions::default()
            },
        )
        .unwrap();

        assert!(legacy.exists(), "legacy k9 backup is never auto-deleted");
        assert_eq!(out.backups.legacy_k9_backup, Some(legacy));
    }

    #[test]
    fn backups_retention_silent_within_retention_limit() {
        let td = scaffold();
        let root = td.path();
        let rendered = root.join(".memhub/backups/rendered");
        fs::create_dir_all(&rendered).unwrap();
        for i in 0..5 {
            touch(&rendered.join(format!("backup-{i}.bak")), 10);
        }

        let out = run_hermetic(root, false).unwrap();

        assert!(out.backups.checked_dirs.is_empty());
        assert!(out.backups.legacy_k9_backup.is_none());
    }

    // -- fail-loud nit: malformed [gc] config gets a breadcrumb --------------

    #[test]
    fn malformed_config_falls_back_to_defaults_with_a_breadcrumb() {
        let td = scaffold();
        let root = td.path();
        let memhub_dir = root.join(".memhub");
        fs::create_dir_all(&memhub_dir).unwrap();
        // Valid TOML, but not a valid ProjectConfig shape (missing the
        // required project_name/log_level fields).
        fs::write(memhub_dir.join("config.toml"), "not_a_real_config = true\n").unwrap();

        let out = run_hermetic(root, false).unwrap();

        // Still resolves to every [gc] opt-in off, never a hard error.
        assert_eq!(out.groups_pruned, 0);
        assert!(
            out.details
                .iter()
                .any(|d| d.contains("did not parse") && d.contains("config.toml")),
            "details: {:?}",
            out.details
        );
    }
}

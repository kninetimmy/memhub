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
//!   balloon, and are structurally never considered.
//! * For each owned stem it keeps every file of the newest-mtime hash
//!   and deletes the older hashes plus their `.fingerprint/<stem>-<hash>`
//!   dirs. Worst case of deleting a superseded hash is that a stale test
//!   binary is rebuilt on the next `cargo test` — Cargo recovers; no
//!   state is corrupted because the current (newest) set is untouched.
//! * Only `target/<profile>/deps/` and `target/<profile>/.fingerprint/`
//!   are in scope. The `incremental/` cache is left alone (deleting it
//!   only slows the next build for no comparable disk win).
//!
//! Pure `std::fs` — OS-agnostic by construction (macOS + Windows).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{MemhubError, Result};

/// Cargo profiles whose `deps/` accumulate stale hashes.
const PROFILES: &[&str] = &["debug", "release"];

/// A cargo metadata hash is lowercase hex; historically 16 chars but be
/// lenient on the low end so older artifacts still match.
const MIN_HASH_LEN: usize = 8;

#[derive(Debug, serde::Serialize)]
pub struct GcOutcome {
    pub root: PathBuf,
    pub dry_run: bool,
    pub removed_files: u64,
    pub removed_dirs: u64,
    pub bytes_freed: u64,
    /// Number of (profile, stem) groups that had at least one stale hash
    /// removed.
    pub groups_pruned: u64,
    /// Human-readable breadcrumbs, one per pruned group plus any
    /// best-effort warnings.
    pub details: Vec<String>,
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
fn owned_stems(root: &Path) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    set.insert("memhub".to_string());
    set.insert("libmemhub".to_string());
    for sub in ["tests", "benches", "examples"] {
        let dir = root.join(sub);
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    set.insert(stem.to_string());
                }
            }
        }
    }
    set
}

/// Split a `deps/` entry name into `(stem, hash)`. The only `-` in a
/// memhub-owned artifact name is the separator before the cargo hash
/// (stems use `_`), so split on the last `-` and validate the tail is a
/// hex hash. Returns `None` for anything that is not a hash-suffixed
/// artifact (e.g. `.cargo-lock`, an un-hashed final binary).
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

/// Prune one `deps/` + `.fingerprint/` pair for one profile, mutating
/// the running totals on `out`.
fn prune_profile(
    profile: &str,
    deps: &Path,
    fingerprint: &Path,
    owned: &std::collections::BTreeSet<String>,
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
        if !owned.contains(&stem) {
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
            let fp = fingerprint.join(format!("{stem}-{hash}"));
            if fp.is_dir() {
                group_bytes += dir_size(&fp);
                group_dirs += 1;
                if !dry_run {
                    if let Err(e) = fs::remove_dir_all(&fp) {
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
            "{profile}/{stem}: kept {keep}, {} {} stale hash(es) ({})",
            if dry_run { "would remove" } else { "removed" },
            by_hash.len() - 1,
            human_bytes(group_bytes),
        ));
    }
}

/// Reclaim superseded build artifacts under `<repo>/target`. When
/// `dry_run` is set nothing is deleted; the outcome reports what would
/// be freed.
pub fn run(start: &Path, dry_run: bool) -> Result<GcOutcome> {
    let root = find_repo_root(start)?;
    let owned = owned_stems(&root);
    let mut out = GcOutcome {
        root: root.clone(),
        dry_run,
        removed_files: 0,
        removed_dirs: 0,
        bytes_freed: 0,
        groups_pruned: 0,
        details: Vec::new(),
    };

    let target = root.join("target");
    for profile in PROFILES {
        let deps = target.join(profile).join("deps");
        if !deps.is_dir() {
            continue;
        }
        let fingerprint = target.join(profile).join(".fingerprint");
        prune_profile(profile, &deps, &fingerprint, &owned, dry_run, &mut out);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::thread::sleep;
    use std::time::Duration;

    fn touch(path: &Path, bytes: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = File::create(path).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
    }

    fn scaffold() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        File::create(root.join("Cargo.toml")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        File::create(root.join("tests/foundation.rs")).unwrap();
        td
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

        let out = run(root, false).unwrap();

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

        let out = run(root, true).unwrap();

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

        let out = run(root, false).unwrap();
        assert_eq!(out.groups_pruned, 0);
        assert!(deps.join("libmemhub-aaaaaaaaaaaaaaaa.rlib").exists());
    }

    #[test]
    fn no_cargo_toml_is_clear_error() {
        let td = tempfile::tempdir().unwrap();
        let err = run(td.path(), false).unwrap_err();
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }
}

//! Install manifest (Wave 5 / decision Q15).
//!
//! `memhub upgrade` resyncs the repo's skill/command templates into the
//! shared, user-global agent dirs (`~/.claude/commands`, `~/.codex/skills`,
//! `~/.config/opencode/{skills,commands}`). Those dirs are *shared* with
//! the user: a hand-authored `~/.claude/commands/recall.md` sits right
//! next to the ones memhub ships. This manifest records the SHA-256 of
//! every file memhub actually writes, so a later resync can tell "a file
//! memhub wrote" from "the user's own" and **never clobber the latter**.
//!
//! ## Fail-safe by construction
//!
//! An absent, unreadable, corrupt, or version-mismatched manifest loads
//! as **empty**. With an empty manifest every pre-existing target file is
//! "unknown" — and unknown is treated as **user-owned**, so it is never
//! overwritten. Only a file memhub can *prove* it wrote (its recorded
//! hash still matches the bytes on disk), or a file whose bytes already
//! equal exactly what memhub is about to ship, is eligible to be
//! (re)written. This is the whole point of the manifest: getting the
//! "ours vs the user's file" call wrong clobbers user data on every
//! future upgrade, so the ambiguous case always resolves to "leave it".
//!
//! The manifest is machine-global derived state, stored beside the global
//! store at `~/.memhub/install-manifest.json`. It is never committed to a
//! repo and is safe to delete (the next resync rebuilds it, conservatively
//! ceding ownership of anything it can no longer prove it wrote).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db;
use crate::retrieval::util::sha256_hex;

const MANIFEST_FILENAME: &str = "install-manifest.json";
/// On-disk format version. A manifest written by a newer memhub (unknown
/// version) loads as empty — fail-safe: we do not trust a shape we do not
/// understand to decide file ownership.
const MANIFEST_VERSION: u32 = 1;

/// `~/.memhub/install-manifest.json`. Machine-global, mirrors the global
/// store's directory. Deliberately **not** gated on the global store
/// existing: skill resync targets `~/.claude` etc. independently of
/// machine-global memory opt-in.
pub fn manifest_path() -> Result<PathBuf> {
    Ok(db::home_dir()?
        .join(db::GLOBAL_MEMHUB_DIRNAME)
        .join(MANIFEST_FILENAME))
}

/// The persisted shape: a version tag plus `target path -> sha256 hex`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    files: BTreeMap<String, String>,
}

/// In-memory install manifest. Keys are the exact target paths memhub
/// writes (lossy string form). The manifest is machine-local, so the
/// string form is stable across runs on the same machine — no
/// canonicalization is needed (and the target may not exist yet at
/// decision time, so canonicalization would be unavailable anyway).
#[derive(Debug, Clone, Default)]
pub struct InstallManifest {
    files: BTreeMap<String, String>,
}

impl InstallManifest {
    /// Load the manifest for this machine, or an **empty** manifest on any
    /// absence/corruption. Empty is the fail-safe: it makes every existing
    /// target file read as user-owned, so nothing is overwritten.
    pub fn load() -> Self {
        match manifest_path() {
            Ok(path) => Self::load_from(&path),
            // Home unresolvable => no manifest we can trust => empty.
            Err(_) => Self::default(),
        }
    }

    /// Load from an explicit path (test seam / internal). Any read or
    /// parse failure, or an unknown format version, yields an empty
    /// manifest — never an error and never a partially-trusted map.
    pub fn load_from(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        match serde_json::from_slice::<ManifestFile>(&bytes) {
            Ok(m) if m.version == MANIFEST_VERSION => Self { files: m.files },
            _ => Self::default(),
        }
    }

    /// The hash memhub last recorded for `key`, if it is tracking that
    /// target at all. `None` means "unknown" — i.e. user territory.
    pub fn recorded(&self, key: &str) -> Option<&str> {
        self.files.get(key).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Every target path memhub currently tracks (used to detect orphans:
    /// files memhub wrote before but no longer ships).
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }

    /// Record that memhub owns `key` with content hash `hash`.
    pub fn record(&mut self, key: String, hash: String) {
        self.files.insert(key, hash);
    }

    /// Persist to `~/.memhub/install-manifest.json`.
    pub fn save(&self) -> Result<()> {
        let path = manifest_path()?;
        self.save_to(&path)
    }

    /// Persist to an explicit path via write-tmp-then-rename, so a crash
    /// mid-write cannot leave a truncated (corrupt) manifest — and even if
    /// it somehow did, a corrupt manifest loads as empty (fail-safe).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let doc = ManifestFile {
            version: MANIFEST_VERSION,
            files: self.files.clone(),
        };
        let json = serde_json::to_vec_pretty(&doc)?;
        let tmp = path.with_file_name(format!("{MANIFEST_FILENAME}.tmp"));
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// What a resync may do with one target file, given what memhub recorded,
/// what is on disk now, and what memhub is about to ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// No file at the target — install fresh.
    Install,
    /// memhub owns it and the shipped bytes differ — safe to overwrite.
    Update,
    /// Owned and already byte-identical to what we ship — record it, but
    /// no write is needed.
    AlreadyCurrent,
    /// Present but **not provably memhub's** — leave it exactly as-is.
    UserOwned,
}

/// The load-bearing "ours vs the user's file" boundary.
///
/// - `recorded`: hash memhub last wrote for this target (from the manifest),
///   or `None` if the manifest does not know this path.
/// - `on_disk`: the bytes currently at the target, or `None` if absent.
/// - `template`: the bytes memhub is about to ship.
///
/// Fail-safe: a present file counts as memhub's **only** when its current
/// bytes hash to what the manifest recorded, or already equal the template.
/// Anything else — unknown to the manifest, or recorded-but-since-modified
/// — is `UserOwned` and must not be touched.
pub fn decide(recorded: Option<&str>, on_disk: Option<&[u8]>, template: &[u8]) -> Decision {
    let Some(disk) = on_disk else {
        return Decision::Install;
    };
    let disk_hash = sha256_hex(disk);
    let template_hash = sha256_hex(template);
    let owned = recorded == Some(disk_hash.as_str()) || disk_hash == template_hash;
    if !owned {
        return Decision::UserOwned;
    }
    if disk_hash == template_hash {
        Decision::AlreadyCurrent
    } else {
        Decision::Update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(bytes: &[u8]) -> String {
        sha256_hex(bytes)
    }

    #[test]
    fn absent_target_installs_regardless_of_manifest() {
        assert_eq!(decide(None, None, b"anything"), Decision::Install);
        assert_eq!(
            decide(Some(&h(b"old")), None, b"anything"),
            Decision::Install,
            "a memhub file the user deleted is re-installed (still ours, still shipped)"
        );
    }

    #[test]
    fn unknown_existing_file_is_user_owned_when_it_differs() {
        // The headline requirement: a hand-authored recall.md memhub never
        // wrote (no manifest entry, content differs from the template) must
        // survive a resync untouched.
        assert_eq!(
            decide(None, Some(b"the user's own recall"), b"memhub template"),
            Decision::UserOwned
        );
    }

    #[test]
    fn unknown_existing_file_identical_to_template_is_adopted() {
        // Bytes already equal what we ship: overwriting is a no-op, so it
        // is safe to adopt into the manifest (either memhub wrote it before
        // the manifest existed, or the user's copy is coincidentally ours).
        assert_eq!(
            decide(None, Some(b"same bytes"), b"same bytes"),
            Decision::AlreadyCurrent
        );
    }

    #[test]
    fn pristine_memhub_file_updates_when_template_changes() {
        let old = b"memhub v1";
        assert_eq!(
            decide(Some(&h(old)), Some(old), b"memhub v2"),
            Decision::Update,
            "recorded hash matches disk => ours => new template lands"
        );
    }

    #[test]
    fn recorded_and_current_is_already_current() {
        let cur = b"memhub v2";
        assert_eq!(
            decide(Some(&h(cur)), Some(cur), cur),
            Decision::AlreadyCurrent
        );
    }

    #[test]
    fn user_modified_memhub_file_becomes_user_owned() {
        // memhub wrote v1 (recorded), the user then edited it. The on-disk
        // bytes no longer match the recorded hash and differ from the new
        // template => fail-safe: cede ownership, never clobber the edit.
        let recorded = h(b"memhub v1");
        assert_eq!(
            decide(Some(&recorded), Some(b"user edited this"), b"memhub v2"),
            Decision::UserOwned
        );
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MANIFEST_FILENAME);
        let mut m = InstallManifest::default();
        m.record("/home/u/.claude/commands/recall.md".to_string(), h(b"x"));
        m.record("/home/u/.codex/skills/recall/SKILL.md".to_string(), h(b"y"));
        m.save_to(&path).expect("save");

        let loaded = InstallManifest::load_from(&path);
        assert_eq!(
            loaded.recorded("/home/u/.claude/commands/recall.md"),
            Some(h(b"x").as_str())
        );
        assert_eq!(
            loaded.recorded("/home/u/.codex/skills/recall/SKILL.md"),
            Some(h(b"y").as_str())
        );
        assert_eq!(loaded.recorded("/nope"), None);
    }

    #[test]
    fn absent_or_corrupt_manifest_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Absent.
        let missing = dir.path().join("does-not-exist.json");
        assert!(InstallManifest::load_from(&missing).is_empty());

        // Corrupt (not JSON at all).
        let garbage = dir.path().join("garbage.json");
        std::fs::write(&garbage, b"\x00not json{{{").expect("write garbage");
        assert!(
            InstallManifest::load_from(&garbage).is_empty(),
            "a corrupt manifest must load empty so every file reads as user-owned"
        );

        // Wrong version => untrusted => empty, even though the JSON parses.
        let wrong_version = dir.path().join("v999.json");
        std::fs::write(
            &wrong_version,
            br#"{"version": 999, "files": {"/x": "deadbeef"}}"#,
        )
        .expect("write wrong version");
        assert!(
            InstallManifest::load_from(&wrong_version).is_empty(),
            "an unknown manifest version must not be trusted to decide ownership"
        );
    }
}

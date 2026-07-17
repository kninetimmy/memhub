//! Shared test-only synchronization helpers for the `upgrade_harness` binary.
//!
//! `db::home_dir()` (which reads `$HOME` / `%USERPROFILE%`) is consulted by
//! `db::discover_paths` **unconditionally as its first line**, before any
//! ancestor-walk short-circuit — so *every* call to `discover_paths` /
//! `open_project` / `open_global` / `global_store_exists` reads these vars,
//! whether or not the specific call ends up depending on the value. Several
//! tests here additionally override `HOME`/`USERPROFILE` (and one also
//! `MEMHUB_REGISTRY_TMP_OK`) with `std::env::set_var`/`remove_var` to
//! redirect machine-global-store resolution at a throwaway tempdir.
//!
//! Before Wave 5 U4 (issue #90) each such test lived in its own `tests/*.rs`
//! binary — its own OS process — so none of this mattered: a read or write
//! in one process can never race a read or write in another, and nothing
//! needed to be undone (the process was about to exit anyway). Consolidating
//! every integration-test file into a handful of shared harness binaries
//! puts these tests in the same process on `cargo test`'s default
//! multi-threaded harness, where two new hazards appear:
//!
//! 1. A **writer** test's `set_var`/`remove_var` racing a **reader** test's
//!    concurrent `var_os` read of the same variable is a data race at the
//!    OS/libc level — exactly why `std::env::set_var`/`remove_var` are
//!    `unsafe` — regardless of whether the reading test's own assertions
//!    end up depending on the value it read. `env_read_lock()` /
//!    `env_write_lock()` below close this for every test that reaches
//!    `home_dir()`, reader or writer, not only the ones whose behavior is
//!    observably sensitive to the value.
//! 2. A writer that overrides `HOME`/`USERPROFILE` and only *partially*
//!    restores them on the way out (several of these tests `remove_var`
//!    the one they set but never restore the other one they also cleared
//!    during setup) permanently corrupts environment resolution for every
//!    later test in the process. `env_write_lock()` returns an RAII guard
//!    that snapshots `HOME` / `USERPROFILE` / `MEMHUB_REGISTRY_TMP_OK` on
//!    entry and restores the exact prior state (present with its prior
//!    value, or absent) on drop, regardless of what the test itself did or
//!    whether it panicked.
//!
//! Implementation: a single `RwLock<()>` rather than a plain `Mutex`, so
//! reader tests (the majority — anything that calls `open_project` et al.
//! without itself mutating `HOME`) can still run concurrently with each
//! other; only a writer excludes everyone (readers and other writers alike)
//! for the — brief — duration of its override.
//!
//! ## Reader-trigger closure — when a NEW test needs `env_read_lock()`
//!
//! `db::home_dir()` is resolved, in-process, by every function below —
//! directly or by calling something else in this list. If a test in this
//! harness calls ANY of these (or a `commands::*` function not listed here
//! that itself calls `open_project`/`init::run`/one of these — the command
//! surface keeps growing, so treat "does it take a repo `Path` and touch
//! `db`/`commands`" as the default-unsafe assumption, not this list as
//! exhaustive-forever), it needs `env_read_lock()` (or `env_lock()` if it
//! also mutates `HOME`/`USERPROFILE`/`MEMHUB_REGISTRY_TMP_OK` itself):
//!
//! - `db::home_dir`, `db::global_db_path`, `db::global_store_exists`
//! - `db::discover_paths`, `db::open_project`, `db::open_global`
//! - `db::registry::{record_open_best_effort, register, list_known}`
//! - `commands::audit_md::run`, `commands::upgrade::check_audit_md`
//! - `commands::global::{begin_write, enable, status}`
//! - `commands::status::run`
//! - `commands::integrations::{enable_k9, disable_k9}`
//! - `commands::pending_write::{insert_pending_write, propose_fact, ...}`
//! - `commands::upgrade::{sync_skills, verify_global, verify_last,
//!   write_last_upgrade, cargo_bin_path, local_bin_shadow, abbrev,
//!   detect_codex_sessions_dir, detect_claude_transcripts_dir}`
//! - `commands::sync::expand_home`, `commands::doctor::run`
//!
//! Genuinely outside the closure (no lock needed) is a short, specific
//! list, not a default: `db::ProjectPaths::for_repo_root` (pure path join),
//! `ProjectConfig::load`/`::save` (plain `fs`+`toml`, no path resolution),
//! `db::init_project` **called on its own** (it never is, in this harness),
//! `commands::init::run` (as of the `sync_md` channel's retirement — audit
//! C5 / task 119 — its only step that reaches `db::home_dir()` was the
//! trailing `sync_md::sync_project` call, now gone; `db::init_project`
//! plus the K9-detection step it also runs are both clean in isolation).
//! Tests that still wrap an `init::run`-only setup in `env_read_lock()` are
//! being conservative, not wrong — the guard is harmless, just no longer
//! load-bearing for that call alone. Also outside the closure: any
//! `memhub::*` function whose signature takes no repo `Path` at all
//! (e.g. `agents_md::generate_agents_md`, `managed_block::parse_managed_block`
//! — pure string transforms), raw `std::fs`/`tempfile` calls on a path the
//! test already has in hand, and spawning the compiled binary as a child
//! process (`std::process::Command`) — a child gets its own independent
//! snapshot of the environment at spawn time, in a separate address space,
//! so nothing it does in-process can race the parent test's threads,
//! however deep into this closure the *child's own* execution goes.
use std::env;
use std::ffi::OsString;
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

static ENV_LOCK: OnceLock<RwLock<()>> = OnceLock::new();

fn lock() -> &'static RwLock<()> {
    ENV_LOCK.get_or_init(|| RwLock::new(()))
}

/// Held for a test that only *reads* `HOME`/`USERPROFILE` resolution
/// in-process (directly, or transitively via `open_project` /
/// `discover_paths` / `open_global` / `global_store_exists` /
/// `check_audit_md` / `db::home_dir`) without mutating it. Takes a shared
/// read guard: any number of reader tests run concurrently with each
/// other, but all are excluded while a writer (see `env_write_lock`) holds
/// the exclusive write guard, so a reader's `var_os` call can never
/// overlap a writer's `set_var`/`remove_var`.
pub struct EnvReadGuard {
    _guard: RwLockReadGuard<'static, ()>,
}

/// Acquire the shared read guard. Bind to a variable held for the whole
/// test body (e.g. `let _env_guard = crate::support::env_read_lock();` as
/// the first line) so it spans every in-process call the test makes.
pub fn env_read_lock() -> EnvReadGuard {
    let guard = lock().read().unwrap_or_else(|poisoned| poisoned.into_inner());
    EnvReadGuard { _guard: guard }
}

/// Held for a test that *mutates* `HOME` / `USERPROFILE` /
/// `MEMHUB_REGISTRY_TMP_OK` (directly with `set_var`/`remove_var`). Takes
/// the exclusive write guard (blocking every reader and every other
/// writer), and restores all three vars to their exact pre-test state on
/// drop.
pub struct EnvWriteGuard {
    _guard: RwLockWriteGuard<'static, ()>,
    home: Option<OsString>,
    userprofile: Option<OsString>,
    registry_tmp_ok: Option<OsString>,
}

/// Acquire the exclusive write guard and snapshot `HOME` / `USERPROFILE` /
/// `MEMHUB_REGISTRY_TMP_OK`. Bind to a variable held for the whole test
/// body (e.g. `let _env_guard = crate::support::env_lock();` as the first
/// line) so both the lock and the restore-on-drop span the entire test,
/// including any early return via `?` or a panic from a failed assertion.
///
/// Recovers from a poisoned lock rather than propagating the poison: the
/// guarded resource is "don't race on / leak ambient env vars", not data
/// that a panicking test could leave in a genuinely corrupt state, so one
/// test's panic must not cascade into spurious failures for every later
/// test that needs this same lock.
pub fn env_lock() -> EnvWriteGuard {
    let guard = lock()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    EnvWriteGuard {
        _guard: guard,
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        registry_tmp_ok: env::var_os("MEMHUB_REGISTRY_TMP_OK"),
    }
}

impl Drop for EnvWriteGuard {
    fn drop(&mut self) {
        // SAFETY: `self._guard` (held until this whole struct finishes
        // dropping, i.e. until after this fn returns) still excludes every
        // reader and writer in this process, so restoring here cannot race
        // a sibling test's read or write of the same variables.
        unsafe {
            match self.home.take() {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match self.userprofile.take() {
                Some(v) => env::set_var("USERPROFILE", v),
                None => env::remove_var("USERPROFILE"),
            }
            match self.registry_tmp_ok.take() {
                Some(v) => env::set_var("MEMHUB_REGISTRY_TMP_OK", v),
                None => env::remove_var("MEMHUB_REGISTRY_TMP_OK"),
            }
        }
    }
}

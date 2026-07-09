//! Shared test-only synchronization helper for the `upgrade_harness` binary.
//!
//! A few tests here resolve the machine home directory (`db::home_dir()`,
//! which reads `$HOME` / `%USERPROFILE%`) and several of them temporarily
//! override it with `std::env::set_var`/`remove_var` to redirect
//! machine-global-store resolution at a throwaway tempdir. Before Wave 5
//! U4 (issue #90) each such test lived in its own `tests/*.rs` binary —
//! its own OS process — so a process-wide env mutation could never race a
//! sibling test, and never needed to be undone (the process was about to
//! exit anyway). Consolidating every integration-test file into a handful
//! of shared harness binaries puts these tests in the same process on
//! `cargo test`'s default multi-threaded harness, where:
//!
//! 1. One test's `HOME` override could race another thread's read of the
//!    same variable (exactly why `std::env::set_var`/`remove_var` are
//!    `unsafe`) — `env_lock()` serializes just the tests that touch this
//!    process-wide state relative to each other; every other test in this
//!    binary is unaffected and keeps running in parallel.
//! 2. A test that overrides `HOME`/`USERPROFILE` and only *partially*
//!    restores them on the way out (several of these tests `remove_var`
//!    the one they set but never restore the other one they also cleared
//!    during setup — harmless when the process died right after, fatal
//!    once a sibling test runs later in the same process) permanently
//!    corrupts environment resolution for every later test — `env_lock()`
//!    returns an RAII guard that snapshots `HOME`/`USERPROFILE` on entry
//!    and restores the exact prior state (set or unset) on drop,
//!    regardless of what the test itself did or whether it panicked.
use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Held for the duration of a test that touches process-wide `HOME` /
/// `USERPROFILE` resolution. Restores both to their exact pre-test state
/// (present with their prior value, or absent) when dropped, then releases
/// the lock — so a test can freely `set_var`/`remove_var` either one
/// without permanently leaking that change to whichever sibling test the
/// harness schedules next in this shared process.
pub struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    home: Option<OsString>,
    userprofile: Option<OsString>,
    registry_tmp_ok: Option<OsString>,
}

/// Acquire the process-wide lock and snapshot `HOME` / `USERPROFILE`. Bind
/// the returned guard to a variable held for the whole test body (e.g.
/// `let _env_guard = crate::support::env_lock();` as the first line) so
/// both the lock and the restore-on-drop span the entire test, including
/// any early return via `?` or a panic from a failed assertion.
///
/// Recovers from a poisoned lock rather than propagating the poison: the
/// guarded resource is "don't race on / leak ambient env vars", not data
/// that a panicking test could leave in a genuinely corrupt state, so one
/// test's panic must not cascade into spurious failures for every later
/// test that needs this same lock.
pub fn env_lock() -> EnvGuard {
    let lock = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    EnvGuard {
        _lock: lock,
        home: env::var_os("HOME"),
        userprofile: env::var_os("USERPROFILE"),
        registry_tmp_ok: env::var_os("MEMHUB_REGISTRY_TMP_OK"),
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: `self._lock` (held until this whole struct finishes
        // dropping, i.e. until after this fn returns) still excludes every
        // other `env_lock()`-holding test in this process, so restoring
        // here cannot race a sibling test's read of the same variables.
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

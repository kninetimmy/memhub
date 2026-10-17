//! Issue #382: `memhub upgrade` embeds every row that lacks an active-model
//! vector in each hybrid-mode project database and in the machine-global
//! store, reports the count per instance, and leaves fts-mode repos alone.
//!
//! Every memhub call is a spawned child with `HOME` and `USERPROFILE` at a
//! tempdir, so the real home, its global store and registry are never
//! reached (same pattern as `global_read_no_migrate.rs`).

use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn memhub(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CARGO_HOME", home.join(".cargo"))
        .env_remove("CARGO_INSTALL_ROOT")
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = memhub(cwd, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn set_mode(repo: &Path, mode: &str) {
    let path = repo.join(".memhub").join("config.toml");
    let raw = std::fs::read_to_string(&path).expect("read config");
    let replaced = raw
        .replace("mode = \"hybrid\"", &format!("mode = \"{mode}\""))
        .replace("mode = \"fts\"", &format!("mode = \"{mode}\""));
    std::fs::write(&path, replaced).expect("write config");
}

fn vectors(db: &Path) -> i64 {
    Connection::open(db)
        .expect("open db")
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .expect("count embeddings")
}

/// A repo whose rows were all written under fts, so none has a vector.
fn unembedded_repo(home: &Path) -> TempDir {
    let repo = tempdir().expect("repo");
    ok(repo.path(), home, &["init"]);
    set_mode(repo.path(), "fts");
    ok(
        repo.path(),
        home,
        &["fact", "add", "build-command", "cargo build"],
    );
    let arch = repo.path().join("arch.md");
    std::fs::write(
        &arch,
        "# Overview\n\nIntro.\n\n## Storage\n\nOne SQLite file per repo.\n",
    )
    .expect("write arch");
    ok(
        repo.path(),
        home,
        &["arch", "set", "--from-file", arch.to_str().expect("utf-8")],
    );
    repo
}

fn finish(home: &Path, also: &[&Path], json: bool) -> String {
    let work = tempdir().expect("work dir");
    let mut args = vec!["upgrade", "--finish", "--no-gc", "--no-hooks"];
    if json {
        args.push("--json");
    }
    let roots: Vec<String> = also.iter().map(|p| p.display().to_string()).collect();
    for r in &roots {
        args.push("--also");
        args.push(r);
    }
    ok(work.path(), home, &args)
}

/// Whether an upgrade instance label names `repo`. Upgrade labels a repo
/// with the path the registry recorded, which is canonicalized: on Windows
/// `\\?\C:\Users\runneradmin\...` for a tempdir spelled
/// `C:\Users\RUNNER~1\...`, on macOS `/private/var/...` for `/var/...`.
/// Only the tempdir's own (unique) last component is the same in every form.
fn names(label: &str, repo: &Path) -> bool {
    Path::new(label).file_name() == repo.file_name()
}

/// The JSON instance row for `repo`, or for the `<global store>` when `None`.
fn row(body: &Value, repo: Option<&Path>) -> Value {
    body["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .find(|r| {
            let label = r["label"].as_str().expect("label");
            match repo {
                Some(p) => names(label, p),
                None => label == "<global store>",
            }
        })
        .unwrap_or_else(|| panic!("no {repo:?} row: {body}"))
        .clone()
}

/// The EMBEDDED cell of the text table's row for `repo` (or the global
/// store when `None`). Columns: INSTANCE, SCHEMA (one token once at head),
/// EMBEDDED, STATUS. The global label `<global store>` holds a space; repo
/// labels (tempdir paths) do not.
fn embedded_cell(text: &str, repo: Option<&Path>) -> String {
    let rest: Vec<&str> = text
        .lines()
        .find_map(|l| {
            let l = l.trim_start();
            match repo {
                None => l
                    .strip_prefix("<global store>")
                    .map(|r| r.split_whitespace().collect()),
                Some(p) => {
                    let mut w = l.split_whitespace();
                    w.next()
                        .filter(|label| names(label, p))
                        .map(|_| w.collect())
                }
            }
        })
        .unwrap_or_else(|| panic!("no {repo:?} line: {text}"));
    rest.get(1)
        .unwrap_or_else(|| panic!("short {repo:?} line: {text}"))
        .to_string()
}

#[test]
fn upgrade_embeds_missing_rows_in_hybrid_repos_and_the_global_store() {
    let home = tempdir().expect("home");
    let hybrid = unembedded_repo(home.path());
    // A global row written from an fts repo has no vector either.
    ok(hybrid.path(), home.path(), &["global", "enable"]);
    ok(
        hybrid.path(),
        home.path(),
        &["fact", "add", "--global", "machine-shell", "bash"],
    );
    set_mode(hybrid.path(), "hybrid");
    let fts = unembedded_repo(home.path());

    let hybrid_db = hybrid.path().join(".memhub").join("project.sqlite");
    let fts_db = fts.path().join(".memhub").join("project.sqlite");
    let global_db = home.path().join(".memhub").join("global.sqlite");
    assert_eq!(vectors(&hybrid_db), 0);
    assert_eq!(vectors(&global_db), 0);

    let stdout = finish(home.path(), &[hybrid.path(), fts.path()], true);
    let body: Value = serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let h = row(&body, Some(hybrid.path()));
    // One fact + two architecture sections.
    assert_eq!(h["embedded"], 3, "{h}");
    let g = row(&body, None);
    assert_eq!(g["embedded"], 1, "{g}");
    let f = row(&body, Some(fts.path()));
    assert!(f["embedded"].is_null(), "fts repo is not filled: {f}");

    assert_eq!(vectors(&hybrid_db), 3);
    assert_eq!(vectors(&global_db), 1);
    assert_eq!(vectors(&fts_db), 0, "fts repo writes no embeddings");
    let status: Value =
        serde_json::from_str(ok(hybrid.path(), home.path(), &["index", "status", "--json"]).trim())
            .expect("index status json");
    assert_eq!(status["missing_count"], 0, "{status}");

    // Nothing left to embed: the hybrid repo's and the global store's
    // EMBEDDED cells read 0; the fts repo's reads `-` (no fill ran).
    let text = finish(home.path(), &[hybrid.path(), fts.path()], false);
    assert!(text.contains("EMBEDDED"), "{text}");
    assert_eq!(embedded_cell(&text, Some(hybrid.path())), "0", "{text}");
    assert_eq!(embedded_cell(&text, None), "0", "{text}");
    assert_eq!(embedded_cell(&text, Some(fts.path())), "-", "{text}");
    assert_eq!(vectors(&hybrid_db), 3);
}

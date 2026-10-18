//! Issue #382: `memhub upgrade` embeds every row that lacks an active-model
//! vector in each hybrid-mode project database and in the machine-global
//! store, reports the count per instance, and leaves fts-mode repos alone;
//! `upgrade --dry-run` previews how many rows each of those stores lacks
//! (issue #393).
//!
//! Every memhub call is a spawned child with `HOME` and `USERPROFILE` at a
//! tempdir, so the real home, its global store and registry are never
//! reached (same pattern as `global_read_no_migrate.rs`).

use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::{Builder, TempDir, tempdir};

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

/// The source-row kinds every test repo seeds, by `index status --json` key.
const SEEDED: [&str; 5] = ["facts", "decisions", "tasks", "doc_chunks", "arch_sections"];

/// A repo whose rows were all written under fts, so none has a vector: a
/// fact, a decision, a task, an ingested doc and two architecture sections.
/// Its path holds a space, which upgrade's labels carry verbatim.
fn unembedded_repo(home: &Path) -> TempDir {
    let repo = Builder::new()
        .prefix("memhub repo ")
        .tempdir()
        .expect("repo");
    ok(repo.path(), home, &["init"]);
    set_mode(repo.path(), "fts");
    ok(
        repo.path(),
        home,
        &["fact", "add", "build-command", "cargo build"],
    );
    ok(
        repo.path(),
        home,
        &[
            "decision",
            "add",
            "Use SQLite",
            "--rationale",
            "one file per repo",
        ],
    );
    ok(repo.path(), home, &["task", "add", "Write the guide"]);
    let doc = repo.path().join("guide.md");
    std::fs::write(&doc, "# Guide\n\nRun the build before a release.\n").expect("write doc");
    ok(
        repo.path(),
        home,
        &["doc", "add", doc.to_str().expect("utf-8")],
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

fn index_status(repo: &Path, home: &Path) -> Value {
    serde_json::from_str(ok(repo, home, &["index", "status", "--json"]).trim())
        .expect("index status json")
}

/// `upgrade --dry-run --json`, run from the source repo as upgrade requires.
fn dry_run(home: &Path, also: &[&Path]) -> Value {
    let mut args = vec!["upgrade", "--dry-run", "--json", "--no-gc", "--no-hooks"];
    let roots: Vec<String> = also.iter().map(|p| p.display().to_string()).collect();
    for r in &roots {
        args.push("--also");
        args.push(r);
    }
    let stdout = ok(Path::new(env!("CARGO_MANIFEST_DIR")), home, &args);
    serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"))
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

/// The EMBEDDED cell of the text table's row labelled `label` (taken from
/// the JSON output, so a label holding spaces, like a repo path with one or
/// `<global store>`, is matched whole). Columns: INSTANCE, SCHEMA (one
/// token once at head), EMBEDDED, STATUS.
fn embedded_cell(text: &str, label: &str) -> String {
    let rest: Vec<&str> = text
        .lines()
        .find_map(|l| {
            l.trim_start()
                .strip_prefix(label)
                .filter(|r| r.starts_with(' '))
                .map(|r| r.split_whitespace().collect())
        })
        .unwrap_or_else(|| panic!("no {label:?} line: {text}"));
    rest.get(1)
        .unwrap_or_else(|| panic!("short {label:?} line: {text}"))
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
    let before = index_status(hybrid.path(), home.path());
    for kind in SEEDED {
        assert!(
            before[kind]["total"].as_i64() >= Some(1),
            "{kind}: {before}"
        );
        assert_eq!(before[kind]["embedded"], 0, "{kind}: {before}");
    }
    let expected = before["missing_count"].as_i64().expect("missing_count");

    // The dry run counts what the fill would embed and writes nothing.
    let preview = dry_run(home.path(), &[hybrid.path(), fts.path()]);
    let h = row(&preview, Some(hybrid.path()));
    assert_eq!(h["would_fill"], true, "{h}");
    assert_eq!(h["lacking_embeddings"], expected, "{h}");
    let f = row(&preview, Some(fts.path()));
    assert_eq!(f["would_fill"], false, "{f}");
    assert!(f["lacking_embeddings"].is_null(), "{f}");
    assert_eq!(
        preview["global_store"]["lacking_embeddings"], 1,
        "{preview}"
    );
    assert_eq!(vectors(&hybrid_db), 0);
    assert_eq!(vectors(&global_db), 0);

    let stdout = finish(home.path(), &[hybrid.path(), fts.path()], true);
    let body: Value = serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let h = row(&body, Some(hybrid.path()));
    assert_eq!(h["embedded"], expected, "{h}");
    let g = row(&body, None);
    assert_eq!(g["embedded"], 1, "{g}");
    let f = row(&body, Some(fts.path()));
    assert!(f["embedded"].is_null(), "fts repo is not filled: {f}");

    assert_eq!(vectors(&hybrid_db), expected);
    assert_eq!(vectors(&global_db), 1);
    assert_eq!(vectors(&fts_db), 0, "fts repo writes no embeddings");
    let status = index_status(hybrid.path(), home.path());
    for kind in SEEDED {
        assert_eq!(
            status[kind]["embedded"], status[kind]["total"],
            "{kind}: {status}"
        );
    }
    assert_eq!(status["missing_count"], 0, "{status}");

    // Nothing left to embed: the hybrid repo's and the global store's
    // EMBEDDED cells read 0; the fts repo's reads `-` (no fill ran).
    let text = finish(home.path(), &[hybrid.path(), fts.path()], false);
    assert!(text.contains("EMBEDDED"), "{text}");
    let label = |repo| {
        row(&body, repo)["label"]
            .as_str()
            .expect("label")
            .to_string()
    };
    assert!(label(Some(hybrid.path())).contains(' '), "{body}");
    assert_eq!(
        embedded_cell(&text, &label(Some(hybrid.path()))),
        "0",
        "{text}"
    );
    assert_eq!(embedded_cell(&text, &label(None)), "0", "{text}");
    assert_eq!(
        embedded_cell(&text, &label(Some(fts.path()))),
        "-",
        "{text}"
    );
    assert_eq!(vectors(&hybrid_db), expected);
}

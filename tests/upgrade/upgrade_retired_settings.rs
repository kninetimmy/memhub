//! Issue #393: `memhub upgrade` removes the retired retrieval settings
//! doctor reports as ignored from each known repo's `config.toml`, deleting
//! only their own lines (CRLF kept), leaves a dotted form in place and says
//! so, and `--dry-run` reports the same without writing.
//!
//! Every memhub call is a spawned child with `HOME` and `USERPROFILE` at a
//! tempdir (same pattern as `upgrade_embedding_fill.rs`). The repos are in
//! fts mode so upgrade loads no embedding model.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
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

fn config(repo: &Path) -> PathBuf {
    repo.join(".memhub").join("config.toml")
}

fn read(repo: &Path) -> String {
    std::fs::read_to_string(config(repo)).expect("read config")
}

/// An initialized repo switched to fts, and its config.toml text then.
fn repo(home: &Path) -> (TempDir, String) {
    let repo = tempdir().expect("repo");
    ok(repo.path(), home, &["init"]);
    let raw = read(repo.path()).replace("mode = \"hybrid\"", "mode = \"fts\"");
    (repo, raw)
}

fn write(repo: &Path, raw: &str) {
    std::fs::write(config(repo), raw).expect("write config");
}

/// `memhub upgrade <phase>` over `repos`; `--finish` runs from a scratch
/// dir, `--dry-run` from the source repo as upgrade requires.
fn upgrade(home: &Path, phase: &str, repos: &[&Path], json: bool) -> String {
    let work = tempdir().expect("work dir");
    let mut args = vec!["upgrade", phase, "--no-gc", "--no-hooks"];
    if json {
        args.push("--json");
    }
    let roots: Vec<String> = repos.iter().map(|p| p.display().to_string()).collect();
    for r in &roots {
        args.push("--also");
        args.push(r);
    }
    let cwd = if phase == "--dry-run" {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    } else {
        work.path()
    };
    ok(cwd, home, &args)
}

fn json(stdout: &str) -> Value {
    serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

/// The `retired_settings` entry naming `repo` (labels are the registry's
/// canonical path; only the tempdir's last component is stable).
fn entry(body: &Value, repo: &Path) -> Value {
    body["retired_settings"]
        .as_array()
        .expect("retired_settings")
        .iter()
        .find(|r| Path::new(r["label"].as_str().expect("label")).file_name() == repo.file_name())
        .unwrap_or_else(|| panic!("no {repo:?} entry: {body}"))
        .clone()
}

fn doctor_ignored(repo: &Path, home: &Path) -> Value {
    let out = memhub(repo, home, &["doctor", "--json"]);
    let body: Value = serde_json::from_slice(&out.stdout).expect("doctor json");
    body["doctor"]["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["id"] == "config_ignored_settings")
        .expect("ignored-settings check")
        .clone()
}

const ALL_FOUR: [&str; 4] = [
    "retrieval.include_stale_by_default",
    "retrieval.scoring.stale_penalty",
    "retrieval.scoring.min_rerank_score",
    "retrieval.scoring.doc_min_rerank_score",
];

#[test]
fn upgrade_removes_retired_settings_and_dry_run_previews_it() {
    let home = tempdir().expect("home");

    // A CRLF file with all four as plain lines (one with a trailing
    // comment) and a commented-out copy; `want` is it without the four.
    let (plain, base) = repo(home.path());
    let commented = base.replace(
        "[retrieval]\n",
        "[retrieval]\n# include_stale_by_default = true\n",
    );
    let with_keys = commented
        .replace(
            "# include_stale_by_default = true\n",
            "# include_stale_by_default = true\ninclude_stale_by_default = false\n",
        )
        .replace(
            "[retrieval.scoring]\n",
            "[retrieval.scoring]\nstale_penalty = 0.3\nmin_rerank_score = 2.0  # old floor\n\
             doc_min_rerank_score = 0.0\n",
        );
    let want = commented.replace('\n', "\r\n");
    write(plain.path(), &with_keys.replace('\n', "\r\n"));

    // The scoring table written as dotted keys under [retrieval].
    let (dotted, base) = repo(home.path());
    let (head, tail) = base.split_once("[retrieval.scoring]\n").expect("table");
    let (block, rest) = tail.split_once("\n\n").expect("table end");
    let keys: String = block.lines().map(|l| format!("scoring.{l}\n")).collect();
    write(
        dotted.path(),
        &format!("{head}{keys}scoring.stale_penalty = 0.3\n\n{rest}"),
    );

    let (plain_raw, dotted_raw) = (read(plain.path()), read(dotted.path()));
    let repos = [plain.path(), dotted.path()];
    let left = json!(["retrieval.scoring.stale_penalty"]);

    // Dry run, both outputs: names what would go, writes nothing.
    let preview = json(&upgrade(home.path(), "--dry-run", &repos, true));
    assert_eq!(entry(&preview, plain.path())["removed"], json!(ALL_FOUR));
    assert_eq!(entry(&preview, dotted.path())["left"], left);
    let text = upgrade(home.path(), "--dry-run", &repos, false);
    let would = format!("would remove retired setting(s) {}", ALL_FOUR.join(", "));
    assert!(text.contains(&would), "{text}");
    assert_eq!(read(plain.path()), plain_raw);
    assert_eq!(read(dotted.path()), dotted_raw);

    let body = json(&upgrade(home.path(), "--finish", &repos, true));
    let p = entry(&body, plain.path());
    assert_eq!(p["removed"], json!(ALL_FOUR), "{p}");
    assert!(p["error"].is_null(), "{p}");
    let d = entry(&body, dotted.path());
    assert_eq!(d["removed"], json!([]), "{d}");
    assert_eq!(d["left"], left, "{d}");

    // Only the four lines went; the CRLF endings and the comment stayed.
    assert_eq!(read(plain.path()), want);
    assert_eq!(read(dotted.path()), dotted_raw);
    let check = doctor_ignored(plain.path(), home.path());
    assert_eq!(check["message"], "no ignored settings", "{check}");
    assert_eq!(doctor_ignored(dotted.path(), home.path())["status"], "warn");

    // A second run has nothing to remove; the text names the dotted leftover.
    let text = upgrade(home.path(), "--finish", &repos, false);
    assert!(!text.contains("removed retired"), "{text}");
    assert!(
        text.contains("left retired setting(s) retrieval.scoring.stale_penalty"),
        "{text}"
    );
}

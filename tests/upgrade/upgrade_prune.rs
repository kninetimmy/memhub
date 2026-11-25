//! Upgrade is exercised only against disposable homes, registries and repos.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use memhub::config::{ProjectConfig, RetrievalMode};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    home: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        for dir in ["tmp", "appdata", "localappdata", ".cargo"] {
            fs::create_dir(home.join(dir)).unwrap();
        }
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        // Dry-run validates the source manifest but never builds or installs.
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"memhub\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        Self { temp, home, source }
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("CARGO_HOME", self.home.join(".cargo"))
            .env_remove("CARGO_INSTALL_ROOT")
            // Never let fixture upgrade or GC inspect real staging shims.
            .env("TEMP", self.home.join("tmp"))
            .env("TMP", self.home.join("tmp"))
            .env("TMPDIR", self.home.join("tmp"))
            .env("APPDATA", self.home.join("appdata"))
            .env("LOCALAPPDATA", self.home.join("localappdata"))
            .env("XDG_CONFIG_HOME", self.home.join("appdata"))
            .env("XDG_CACHE_HOME", self.home.join("localappdata"))
            .env("MEMHUB_REGISTRY_TMP_OK", "1")
            .env("MEMHUB_LOG", "off")
            .output()
            .unwrap()
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn repo(&self, name: &str) -> PathBuf {
        let repo = self.temp.path().join(name);
        fs::create_dir(&repo).unwrap();
        self.ok(&repo, &["init"]);
        let path = repo.join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&path).unwrap();
        cfg.retrieval.mode = RetrievalMode::Fts;
        cfg.retrieval.use_reranker = false;
        cfg.save(&path).unwrap();
        repo
    }

    fn upgrade(&self, phase: &str, flags: &[&str]) -> Output {
        let mut args = vec!["upgrade", phase, "--no-gc", "--no-skills", "--no-hooks"];
        args.extend_from_slice(flags);
        self.run(&self.source, &args)
    }

    fn upgrade_ok(&self, phase: &str, flags: &[&str]) -> String {
        let out = self.upgrade(phase, flags);
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn register(&self, repo: &Path) {
        self.ok(repo, &["global", "enable"]);
        self.ok(repo, &["global", "disable"]);
    }
}

fn db(repo: &Path) -> Connection {
    Connection::open(repo.join(".memhub/project.sqlite")).unwrap()
}

fn body(stdout: &str) -> Value {
    serde_json::from_str(stdout.lines().next().unwrap()).unwrap()
}

fn entry(body: &Value, repo: &Path) -> Value {
    body["prune_scan"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| Path::new(r["label"].as_str().unwrap()).file_name() == repo.file_name())
        .unwrap_or_else(|| panic!("no {repo:?} scan: {body}"))
        .clone()
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        // SQLite readers may create an empty WAL and update shared-memory
        // read marks. Neither is stored data; retain nonempty WAL bytes.
        let name = path.file_name().unwrap().to_str().unwrap();
        if name == "project.sqlite-shm"
            || (name == "project.sqlite-wal" && path.metadata().unwrap().len() == 0)
        {
            continue;
        }
        if path.is_dir() {
            result.extend(files(&path));
        } else {
            result.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    result
}

fn memory(repo: &Path) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let conn = db(repo);
    [
        "facts",
        "decisions",
        "tasks",
        "documents",
        "doc_chunks",
        "project_arch",
        "arch_sections",
        "project_state",
        "session_notes",
        "pending_writes",
        "prune_dismissals",
        "writes_log",
    ]
    .map(|table| {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = stmt.column_count();
        stmt.query_map([], |row| {
            (0..columns)
                .map(|i| row.get(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    })
    .into()
}

#[test]
fn upgrade_prune_reports_observations_without_acting() {
    let f = Fixture::new();
    let repo = f.repo("repo with spaces");
    f.register(&repo);
    let empty = f.repo("empty");
    f.register(&empty);
    let document = repo.join("guide.md");
    fs::write(&document, "# Before").unwrap();
    f.ok(&repo, &["doc", "add", document.to_str().unwrap()]);
    fs::write(&document, "# Changed").unwrap();
    {
        let conn = db(&repo);
        for i in 0..28 {
            conn.execute(
                "INSERT INTO facts(key,value,verified_at) VALUES (?1,'old','2000-01-01')",
                [format!("old-{i}")],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO tasks(title,updated_at) VALUES ('idle','2000-01-01')",
            [],
        )
        .unwrap();
    }
    let scan = body(&f.ok(&repo, &["prune", "scan", "--json"]));
    let dismissed = scan["prune_scan"]["items"][0]["id"].as_str().unwrap();
    f.ok(&repo, &["prune", "dismiss", dismissed]);
    let backups = repo.join(".memhub/backups/rendered");
    fs::create_dir_all(&backups).unwrap();
    for i in 0..21 {
        fs::write(backups.join(format!("{i:02}.bak")), b"backup").unwrap();
    }
    let expected = body(&f.ok(&repo, &["prune", "scan", "--json"]))["prune_scan"].clone();
    assert_eq!(expected["counts"]["stale_fact"]["total"], 27);
    assert_eq!(expected["counts"]["stale_fact"]["overflow"], 2);
    let before_memory = memory(&repo);
    let before_files = files(&repo);
    let before_empty = files(&empty);

    let preview = body(&f.upgrade_ok("--dry-run", &["--json"]));
    let actual = entry(&preview, &repo);
    assert_scan(&actual, &expected, "before_upgrade");
    assert_eq!(entry(&preview, &empty)["total"], 0);
    let text = f.upgrade_ok("--dry-run", &[]);
    assert!(text.contains("prune scan (before upgrade)"), "{text}");
    assert!(text.contains("Stale facts: 27"), "{text}");
    assert!(text.contains("Idle tasks: 1"), "{text}");
    assert!(text.contains("Out-of-date documents: 1"), "{text}");
    assert!(text.contains("Surplus backups: 1"), "{text}");
    assert!(text.contains("run /prune in this repo"), "{text}");
    assert_eq!(files(&repo), before_files, "dry-run changed repo files");
    assert_eq!(files(&empty), before_empty);

    let upgraded = body(&f.upgrade_ok("--finish", &["--json"]));
    assert_scan(&entry(&upgraded, &repo), &expected, "after_upgrade");
    assert_eq!(entry(&upgraded, &empty)["total"], 0);
    let text = f.upgrade_ok("--finish", &[]);
    assert!(text.contains("prune scan (after upgrade)"), "{text}");
    assert!(text.contains("Stale facts: 27"), "{text}");
    assert!(text.contains("run /prune in this repo"), "{text}");
    assert_eq!(memory(&repo), before_memory, "scan changed memory rows");
    for (path, bytes) in before_files {
        if path.extension().is_some_and(|ext| ext == "sqlite") {
            continue; // The pre-existing maintained upgrade open writes metadata.
        }
        assert_eq!(
            fs::read(path).unwrap(),
            bytes,
            "upgrade deleted/edited a file"
        );
    }

    for phase in ["--dry-run", "--finish"] {
        let skipped = body(&f.upgrade_ok(phase, &["--json", "--no-prune-scan"]));
        assert_eq!(skipped["prune_scan"], json!([]));
        let text = f.upgrade_ok(phase, &["--no-prune-scan"]);
        assert!(!text.contains("prune scan"), "{text}");
        assert!(!text.contains("Stale facts:"), "{text}");
    }
}

#[test]
fn upgrade_prune_scan_failure_is_local_and_does_not_change_upgrade_outcome() {
    let f = Fixture::new();
    let broken_scan = f.repo("a-broken-scan");
    let healthy = f.repo("z-healthy");
    f.register(&broken_scan);
    f.register(&healthy);
    db(&broken_scan)
        .execute_batch("DROP TABLE prune_dismissals")
        .unwrap();
    for phase in ["--dry-run", "--finish"] {
        let report = body(&f.upgrade_ok(phase, &["--json"]));
        let error = entry(&report, &broken_scan);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("prune_dismissals")
        );
        assert!(error.get("counts").is_none(), "{error}");
        assert_eq!(entry(&report, &healthy)["total"], 0);
        let text = f.upgrade_ok(phase, &[]);
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("z-healthy: 0 item(s)"), "{text}");
    }
    // An unrelated migration failure still fails upgrade, with or without scans.
    db(&healthy)
        .execute(
            "INSERT INTO schema_migrations(version) VALUES ('9999_future')",
            [],
        )
        .unwrap();
    for flags in [vec!["--json"], vec!["--json", "--no-prune-scan"]] {
        let out = f.upgrade("--finish", &flags);
        assert_eq!(out.status.code(), Some(1));
        let report = body(&String::from_utf8(out.stdout).unwrap());
        assert!(
            report["instances"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["status"] == "error")
        );
    }
}

fn assert_scan(summary: &Value, scan: &Value, timing: &str) {
    assert_eq!(summary["timing"], timing);
    assert_eq!(summary["total"], scan["total"]);
    assert_eq!(
        summary["counts"].as_object().unwrap().len(),
        scan["counts"].as_object().unwrap().len()
    );
    for (category, counts) in scan["counts"].as_object().unwrap() {
        assert_eq!(summary["counts"][category], counts["total"], "{category}");
    }
    for warning in ["backup_warning", "similarity_warning"] {
        assert_eq!(summary.get(warning), scan.get(warning));
    }
    assert!(summary["review"].as_str().unwrap().contains("/prune"));
    assert!(summary.get("error").is_none(), "{summary}");
}

#[test]
fn upgrade_prune_after_observes_final_machine_outcome_without_changing_it() {
    for failed in [false, true] {
        let f = Fixture::new();
        let observer = f.repo("observer");
        let other = f.repo("other");
        if failed {
            db(&other)
                .execute(
                    "INSERT INTO schema_migrations(version) VALUES ('9999_future')",
                    [],
                )
                .unwrap();
        }
        let status_file = f.home.join(".memhub/last_upgrade.json");
        fs::create_dir_all(status_file.parent().unwrap()).unwrap();
        let prior = json!({
            "ok": true, "state": "ok", "summary": "prior upgrade", "unix_ms": 0
        })
        .to_string();
        fs::write(&status_file, &prior).unwrap();
        f.ok(&observer, &["doc", "add", status_file.to_str().unwrap()]);
        let before = body(&f.ok(&observer, &["prune", "scan", "--json"]))["prune_scan"].clone();
        assert_eq!(before["counts"]["out_of_date_document"]["total"], 0);
        let memory_before = memory(&observer);
        let flags = [
            "--json",
            "--also",
            observer.to_str().unwrap(),
            "--also",
            other.to_str().unwrap(),
        ];
        let preview = body(&f.upgrade_ok("--dry-run", &flags));
        assert_scan(&entry(&preview, &observer), &before, "before_upgrade");
        assert_eq!(fs::read_to_string(&status_file).unwrap(), prior);

        let actual = f.upgrade("--finish", &flags);
        let expected_code = if failed { 1 } else { 0 };
        assert_eq!(actual.status.code(), Some(expected_code));
        let stdout = String::from_utf8(actual.stdout).unwrap();
        let status_bytes = fs::read(&status_file).unwrap();
        let status: Value = serde_json::from_slice(&status_bytes).unwrap();
        assert_eq!(status["ok"], !failed);
        assert_eq!(status["state"], if failed { "failed" } else { "ok" });
        let final_line = format!(
            "memhub upgrade: {} — {}",
            if failed { "FAILED" } else { "SUCCESS" },
            status["summary"].as_str().unwrap()
        );
        assert_eq!(stdout.lines().last().unwrap(), final_line);
        let verified = f.upgrade("--verify-last", &[]);
        assert_eq!(verified.status.code(), Some(expected_code));
        assert_eq!(
            String::from_utf8(verified.stdout).unwrap().trim(),
            final_line
        );

        let after = body(&f.ok(&observer, &["prune", "scan", "--json"]))["prune_scan"].clone();
        assert_eq!(after["counts"]["out_of_date_document"]["total"], 1);
        assert_scan(&entry(&body(&stdout), &observer), &after, "after_upgrade");
        assert_eq!(fs::read(&status_file).unwrap(), status_bytes);
        assert_eq!(memory(&observer), memory_before);
    }
}

#[test]
fn upgrade_prune_before_and_after_observe_another_selected_roots_cleanup() {
    let f = Fixture::new();
    let observer = f.repo("a-observer");
    let owner = f.repo("z-owner");
    // --also order puts the observer first, so inline finish scans are wrong.
    let flags = [
        "--json",
        "--also",
        observer.to_str().unwrap(),
        "--also",
        owner.to_str().unwrap(),
    ];
    let twin = owner.join(".memhub/rendered/AGENTS.md");
    fs::create_dir_all(twin.parent().unwrap()).unwrap();
    fs::write(
        &twin,
        "# Project state (machine-local, generated by memhub)\nOld state.\n",
    )
    .unwrap();
    f.ok(&observer, &["doc", "add", twin.to_str().unwrap()]);
    let before = body(&f.ok(&observer, &["prune", "scan", "--json"]))["prune_scan"].clone();
    assert_eq!(before["counts"]["out_of_date_document"]["total"], 0);
    let before_memory = memory(&observer);
    let owner_memory = memory(&owner);
    let before_files = files(&observer);
    let owner_files = files(&owner);
    let preview = body(&f.upgrade_ok("--dry-run", &flags));
    assert_scan(&entry(&preview, &observer), &before, "before_upgrade");
    assert_eq!(files(&observer), before_files);
    assert_eq!(files(&owner), owner_files);

    let actual = body(&f.upgrade_ok("--finish", &flags));
    let order: Vec<_> = actual["instances"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| Path::new(r["label"].as_str().unwrap()).file_name())
        .collect();
    assert!(
        order
            .iter()
            .position(|n| *n == observer.file_name().unwrap())
            .unwrap()
            < order
                .iter()
                .position(|n| *n == owner.file_name().unwrap())
                .unwrap()
    );
    assert!(
        !twin.exists(),
        "ordinary upgrade did not remove its generated twin"
    );
    let after = body(&f.ok(&observer, &["prune", "scan", "--json"]))["prune_scan"].clone();
    assert_eq!(after["counts"]["out_of_date_document"]["total"], 1);
    assert_scan(&entry(&actual, &observer), &after, "after_upgrade");
    assert_ne!(
        entry(&preview, &observer)["counts"],
        entry(&actual, &observer)["counts"]
    );
    assert_eq!(memory(&observer), before_memory);
    assert_eq!(memory(&owner), owner_memory);
}

#[test]
fn upgrade_prune_unavailable_legacy_and_nested_roots_do_not_migrate_or_scan_ancestors() {
    let f = Fixture::new();
    let repo = f.repo("legacy");
    let nested = repo.join("uninitialized-child");
    fs::create_dir(&nested).unwrap();
    let conn = db(&repo);
    conn.execute_batch(
        "DROP TABLE prune_dismissals;
         ALTER TABLE facts DROP COLUMN retired_at;
         ALTER TABLE facts DROP COLUMN retired_reason;
         ALTER TABLE documents DROP COLUMN archived_at;
         ALTER TABLE documents DROP COLUMN archived_reason;
         DELETE FROM schema_migrations WHERE version >= '0026';
         UPDATE projects SET schema_version = '0025_arch_sections';
         INSERT INTO facts(key,value,verified_at) VALUES ('legacy','old','2000-01-01');",
    )
    .unwrap();
    let before = files(&repo);
    let flags = [
        "--json",
        "--also",
        repo.to_str().unwrap(),
        "--also",
        nested.to_str().unwrap(),
    ];
    let preview = body(&f.upgrade_ok("--dry-run", &flags));
    let legacy = entry(&preview, &repo);
    assert_eq!(legacy["timing"], "before_upgrade");
    assert!(
        legacy["error"]
            .as_str()
            .unwrap()
            .contains("0025_arch_sections")
    );
    assert!(legacy.get("counts").is_none());
    let child = entry(&preview, &nested);
    assert!(child["error"].as_str().unwrap().contains("selected root"));
    assert!(child.get("total").is_none());
    assert_eq!(files(&repo), before);
    assert!(!repo.join(".memhub/backups").exists());
    drop(conn);
    let actual = body(&f.upgrade_ok("--finish", &flags));
    let direct = body(&f.ok(&repo, &["prune", "scan", "--json"]))["prune_scan"].clone();
    assert_scan(&entry(&actual, &repo), &direct, "after_upgrade");
    assert_eq!(direct["counts"]["stale_fact"]["total"], 1);
    assert!(
        actual["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "migrated")
    );
    assert!(
        !actual["prune_scan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| Path::new(r["label"].as_str().unwrap()).file_name() == nested.file_name())
    );
}

#[test]
fn upgrade_prune_uses_full_scanner_settings_and_preserves_partial_warnings() {
    let f = Fixture::new();
    let repo = f.repo("similarity-and-warnings");
    let config = repo.join(".memhub/config.toml");
    let mut cfg = ProjectConfig::load(&config).unwrap();
    cfg.retrieval.use_reranker = true;
    cfg.retrieval.fact_stale_after_days = 10;
    cfg.save(&config).unwrap();
    db(&repo)
        .execute_batch(
            "INSERT INTO facts(key,value,verified_at) VALUES
         ('database','We will run a Postgres server to hold project memory.',CURRENT_TIMESTAMP),
         ('datastore','We will run a Postgres server to hold project memory.',CURRENT_TIMESTAMP),
         ('recent','A distinct old claim.',datetime('now','-15 days'));",
        )
        .unwrap();
    // A nondirectory backup root makes that category partial, not the whole scan.
    fs::write(repo.join(".memhub/backups"), "not a directory").unwrap();
    let expected = body(&f.ok(&repo, &["prune", "scan", "--json"]))["prune_scan"].clone();
    assert_eq!(expected["counts"]["duplicate_facts"]["total"], 1);
    assert_eq!(expected["counts"]["stale_fact"]["total"], 1);
    assert!(expected["backup_warning"].is_string());
    let memory_before = memory(&repo);
    for (phase, timing) in [
        ("--dry-run", "before_upgrade"),
        ("--finish", "after_upgrade"),
    ] {
        let flags = ["--json", "--also", repo.to_str().unwrap()];
        let actual = body(&f.upgrade_ok(phase, &flags));
        assert_scan(&entry(&actual, &repo), &expected, timing);
        let text = f.upgrade_ok(phase, &["--also", repo.to_str().unwrap()]);
        assert!(text.contains("Near-duplicate facts: 1"), "{text}");
        assert!(
            text.contains(expected["backup_warning"].as_str().unwrap()),
            "{text}"
        );
    }
    assert_eq!(memory(&repo), memory_before);
}

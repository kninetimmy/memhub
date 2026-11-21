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
            // Upgrade sweeps staging shims even on Windows dry-runs.
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
fn upgrade_prune_reports_all_registered_repos_and_dry_run_matches_without_acting() {
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
    assert_eq!(actual["total"], expected["total"]);
    for (category, counts) in expected["counts"].as_object().unwrap() {
        assert_eq!(actual["counts"][category], counts["total"]);
    }
    assert_eq!(entry(&preview, &empty)["total"], 0);
    let text = f.upgrade_ok("--dry-run", &[]);
    assert!(text.contains("Stale facts: 27"), "{text}");
    assert!(text.contains("Idle tasks: 1"), "{text}");
    assert!(text.contains("Out-of-date documents: 1"), "{text}");
    assert!(text.contains("Surplus backups: 1"), "{text}");
    assert!(text.contains("run /prune in this repo"), "{text}");
    assert_eq!(files(&repo), before_files, "dry-run changed repo files");
    assert_eq!(files(&empty), before_empty);

    let upgraded = body(&f.upgrade_ok("--finish", &["--json"]));
    assert_eq!(entry(&upgraded, &repo), actual);
    assert_eq!(entry(&upgraded, &empty), entry(&preview, &empty));
    let text = f.upgrade_ok("--finish", &[]);
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
        assert!(!text.contains("prune scan:"), "{text}");
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
        assert!(text.contains("warning: scan failed"), "{text}");
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

#[test]
fn upgrade_prune_dry_run_previews_legacy_schema_in_memory_only() {
    let f = Fixture::new();
    let repo = f.repo("legacy");
    // Restore the pre-lifecycle columns and ledger so the real upgrade must
    // apply 0026 and 0027. Preserve the WAL while preview copies committed rows.
    let conn = db(&repo);
    conn.execute_batch(
        "DROP TABLE prune_dismissals;
         ALTER TABLE facts DROP COLUMN retired_at;
         ALTER TABLE facts DROP COLUMN retired_reason;
         ALTER TABLE documents DROP COLUMN archived_at;
         ALTER TABLE documents DROP COLUMN archived_reason;
         DELETE FROM schema_migrations WHERE version >= '0026';
         UPDATE projects SET schema_version = '0025_arch_sections';
         INSERT INTO facts(key,value,verified_at) VALUES ('legacy','old','2000-01-01');
         INSERT INTO tasks(title,updated_at) VALUES ('legacy task','2000-01-01');",
    )
    .unwrap();
    assert!(
        repo.join(".memhub/project.sqlite-wal")
            .metadata()
            .unwrap()
            .len()
            > 0
    );
    let before = files(&repo);
    let args = ["--json", "--also", repo.to_str().unwrap()];
    let preview = body(&f.upgrade_ok("--dry-run", &args));
    let summary = entry(&preview, &repo);
    assert_eq!(summary["counts"]["stale_fact"], 1);
    assert_eq!(summary["counts"]["idle_task"], 1);
    assert_eq!(
        files(&repo),
        before,
        "preview changed the legacy DB or files"
    );
    assert!(!repo.join(".memhub/backups").exists());
    drop(conn);
    let upgraded = body(&f.upgrade_ok("--finish", &args));
    assert_eq!(entry(&upgraded, &repo), summary);
    assert!(
        upgraded["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "migrated")
    );
}

#[test]
fn upgrade_prune_preview_accounts_for_source_gc_only_when_permitted() {
    let f = Fixture::new();
    f.ok(&f.source, &["init"]);
    let config = f.source.join(".memhub/config.toml");
    let mut cfg = ProjectConfig::load(&config).unwrap();
    cfg.retrieval.mode = RetrievalMode::Fts;
    cfg.gc.delete_stale_backups = true;
    cfg.save(&config).unwrap();
    let other = f.repo("registered-not-gced");
    f.register(&other);
    for repo in [&f.source, &other] {
        let backups = repo.join(".memhub/backups/rendered");
        fs::create_dir_all(&backups).unwrap();
        for i in 0..21 {
            fs::write(backups.join(format!("{i:02}.bak")), b"backup").unwrap();
        }
    }
    let before = files(&f.source);
    let before_memory = memory(&f.source);
    let skipped = body(&f.upgrade_ok("--dry-run", &["--json"]));
    assert_eq!(entry(&skipped, &f.source)["counts"]["surplus_backups"], 1);

    let args = [
        "upgrade",
        "--dry-run",
        "--no-skills",
        "--no-hooks",
        "--json",
    ];
    let preview = body(&f.ok(&f.source, &args));
    assert_eq!(entry(&preview, &f.source)["counts"]["surplus_backups"], 0);
    assert_eq!(entry(&preview, &other)["counts"]["surplus_backups"], 1);
    assert_eq!(files(&f.source), before, "preview performed GC");
    assert_eq!(memory(&f.source), before_memory);

    cfg.gc.delete_stale_backups = false;
    cfg.save(&config).unwrap();
    let report_only = body(&f.ok(&f.source, &args));
    assert_eq!(
        entry(&report_only, &f.source)["counts"]["surplus_backups"],
        1
    );
    cfg.gc.delete_stale_backups = true;
    cfg.save(&config).unwrap();

    // Exercise the real orchestrate -> finish ordering without a cargo install:
    // orchestrate calls this same GC entry point before relaunching --finish.
    f.ok(&f.source, &["gc", "--json"]);
    let actual = body(&f.upgrade_ok("--finish", &["--json"]));
    assert_eq!(entry(&actual, &f.source), entry(&preview, &f.source));
    assert_eq!(entry(&actual, &other), entry(&preview, &other));
    assert_eq!(
        fs::read_dir(f.source.join(".memhub/backups/rendered"))
            .unwrap()
            .count(),
        20
    );
    assert_eq!(memory(&f.source), before_memory);
}

#[test]
fn upgrade_prune_preview_marks_ingested_generated_twins_missing_even_without_gc() {
    let f = Fixture::new();
    let repo = f.repo("generated-twin");
    f.register(&repo);
    let rendered = repo.join(".memhub/rendered");
    fs::create_dir_all(&rendered).unwrap();
    let twin = rendered.join("AGENTS.md");
    fs::write(
        &twin,
        "# Project state (machine-local, generated by memhub)\n\nOld generated state.\n",
    )
    .unwrap();
    f.ok(&repo, &["doc", "add", twin.to_str().unwrap()]);
    assert_eq!(
        body(&f.ok(&repo, &["prune", "scan", "--json"]))["prune_scan"]["total"],
        0
    );
    let before = files(&repo);
    let before_memory = memory(&repo);
    let preview = body(&f.upgrade_ok("--dry-run", &["--json"]));
    assert_eq!(entry(&preview, &repo)["counts"]["out_of_date_document"], 1);
    assert_eq!(files(&repo), before, "preview removed the twin");
    assert_eq!(memory(&repo), before_memory);
    let actual = body(&f.upgrade_ok("--finish", &["--json"]));
    assert_eq!(entry(&actual, &repo), entry(&preview, &repo));
    assert!(!twin.exists());
    assert_eq!(memory(&repo), before_memory);
}

#[test]
fn upgrade_prune_preview_initializes_a_blank_database_without_a_ledger_only_in_memory() {
    let f = Fixture::new();
    let repo = f.temp.path().join("blank");
    fs::create_dir_all(repo.join(".memhub")).unwrap();
    drop(db(&repo));
    let config = repo.join(".memhub/config.example.toml");
    let mut cfg = ProjectConfig::default_for_repo_name("blank");
    cfg.retrieval.mode = RetrievalMode::Fts;
    cfg.save(&config).unwrap();
    let before = files(&repo);
    let args = ["--json", "--also", repo.to_str().unwrap()];
    let preview = body(&f.upgrade_ok("--dry-run", &args));
    assert_eq!(entry(&preview, &repo)["total"], 0);
    assert_eq!(files(&repo), before, "preview initialized the source DB");
    assert!(!repo.join(".memhub/config.toml").exists());
    let actual = body(&f.upgrade_ok("--finish", &args));
    assert_eq!(entry(&actual, &repo), entry(&preview, &repo));
    assert!(actual["instances"].as_array().unwrap().iter().any(|r| {
        Path::new(r["label"].as_str().unwrap()).file_name() == repo.file_name()
            && r["status"] == "migrated"
    }));
}

#[test]
fn upgrade_prune_preview_uses_rewritten_config_for_settings_and_ingested_bytes() {
    for retired_value in ["true", "\"old untyped value\""] {
        let f = Fixture::new();
        let repo = f.repo("retired-config");
        f.register(&repo);
        let config = repo.join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&config).unwrap();
        cfg.retrieval.include_docs_in_default = true;
        cfg.save(&config).unwrap();
        let original = fs::read_to_string(&config).unwrap();
        let with_retired = original.replace(
            "[retrieval]\n",
            "[retrieval]\ninclude_stale_by_default = true\n",
        );
        fs::write(&config, &with_retired).unwrap();
        f.ok(&repo, &["doc", "add", config.to_str().unwrap()]);
        let scan = body(&f.ok(&repo, &["prune", "scan", "--json"]));
        assert_eq!(scan["prune_scan"]["total"], 0, "{scan}");
        fs::write(
            &config,
            with_retired.replace(
                "include_stale_by_default = true",
                &format!("include_stale_by_default = {retired_value}"),
            ),
        )
        .unwrap();
        db(&repo)
            .execute(
                "INSERT INTO facts(key,value,verified_at) VALUES ('old','value','2000-01-01')",
                [],
            )
            .unwrap();
        let before = files(&repo);
        let before_memory = memory(&repo);
        let preview = body(&f.upgrade_ok("--dry-run", &["--json"]));
        let summary = entry(&preview, &repo);
        assert_eq!(summary["counts"]["stale_fact"], 1, "{summary}");
        assert_eq!(summary["counts"]["out_of_date_document"], 1, "{summary}");
        assert_eq!(files(&repo), before);
        assert_eq!(memory(&repo), before_memory);
        let actual = body(&f.upgrade_ok("--finish", &["--json"]));
        assert_eq!(entry(&actual, &repo), summary);
        assert_eq!(fs::read_to_string(&config).unwrap(), original);
        assert_eq!(memory(&repo), before_memory);
    }
}

#[test]
fn upgrade_prune_preview_observes_cross_repo_cleanup_in_registry_and_also_order() {
    for (owner_name, observer_name, expected) in
        [("a-owner", "z-observer", 2), ("z-owner", "a-observer", 0)]
    {
        let f = Fixture::new();
        // Create the owner before the global registry exists; only --also
        // registers it, before real upgrade enumerates the sorted registry.
        let owner = f.repo(owner_name);
        let observer = f.repo(observer_name);
        f.register(&observer);
        let config = owner.join(".memhub/config.toml");
        let raw = fs::read_to_string(&config).unwrap().replace(
            "[retrieval]\n",
            "[retrieval]\ninclude_stale_by_default = true\n",
        );
        fs::write(&config, raw).unwrap();
        let twin = owner.join(".memhub/rendered/AGENTS.md");
        fs::create_dir_all(twin.parent().unwrap()).unwrap();
        fs::write(
            &twin,
            "# Project state (machine-local, generated by memhub)\n\nOld state.\n",
        )
        .unwrap();
        for document in [&config, &twin] {
            f.ok(&observer, &["doc", "add", document.to_str().unwrap()]);
        }
        let before = files(&owner);
        let before_observer = files(&observer);
        let before_memory = memory(&observer);
        let args = ["--json", "--also", owner.to_str().unwrap()];
        let preview = body(&f.upgrade_ok("--dry-run", &args));
        let summary = entry(&preview, &observer);
        assert_eq!(
            summary["counts"]["out_of_date_document"], expected,
            "{preview}"
        );
        assert_eq!(files(&owner), before);
        assert_eq!(files(&observer), before_observer);
        let actual = body(&f.upgrade_ok("--finish", &args));
        assert_eq!(entry(&actual, &observer), summary);
        assert!(!twin.exists());
        assert_eq!(memory(&observer), before_memory);
    }
}

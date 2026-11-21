use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("fixture");
        fs::create_dir(&repo).unwrap();
        let this = Self { temp, repo };
        this.ok(&["init"]);
        let config = this.repo.join(".memhub/config.toml");
        let mut cfg = memhub::config::ProjectConfig::load(&config).unwrap();
        cfg.retrieval.mode = memhub::config::RetrievalMode::Fts;
        cfg.retrieval.fact_stale_after_days = 10;
        cfg.save(&config).unwrap();
        this
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
            .current_dir(&self.repo)
            .env("MEMHUB_LOG", "off")
            .env("HOME", self.temp.path())
            .env("USERPROFILE", self.temp.path())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Vec<u8> {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.ok(args)).unwrap()
    }

    fn scan(&self) -> Value {
        self.json(&["prune", "scan", "--json"])["prune_scan"].clone()
    }

    fn db(&self) -> Connection {
        Connection::open(self.repo.join(".memhub/project.sqlite")).unwrap()
    }

    fn document(&self, name: &str, new_bytes: Option<&[u8]>, archived: bool) {
        let path = self.temp.path().join(name); // Deliberately external to the repo.
        fs::write(&path, "# Original").unwrap();
        let metadata = self.json(&[
            "doc",
            "add",
            "--title=-custom",
            "--json",
            path.to_str().unwrap(),
        ]);
        if archived {
            self.ok(&[
                "doc",
                "archive",
                &metadata["id"].to_string(),
                "--reason",
                "fixture",
            ]);
        }
        if let Some(new_bytes) = new_bytes {
            fs::write(&path, new_bytes).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }

    fn backups(&self, dir: &str, count: u64) -> PathBuf {
        let path = self.repo.join(".memhub/backups").join(dir);
        fs::create_dir_all(&path).unwrap();
        for i in 0..count {
            let backup = path.join(format!("{i:02}.bak"));
            fs::write(&backup, b"a").unwrap();
            let file = File::options().write(true).open(&backup).unwrap();
            let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i);
            file.set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        }
        path
    }
}

fn items(scan: &Value) -> &[Value] {
    scan["items"].as_array().unwrap()
}

fn row_item<'a>(report: &'a Value, category: &str, row_id: i64) -> &'a Value {
    items(report)
        .iter()
        .find(|item| item["category"] == category && item["subjects"][0]["id"] == row_id)
        .unwrap()
}

fn action<'a>(item: &'a Value, action: &str) -> Vec<&'a str> {
    let argv = item["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == action)
        .unwrap()["command"]
        .as_array()
        .unwrap();
    assert_eq!(argv[0], "memhub");
    argv[1..].iter().map(|arg| arg.as_str().unwrap()).collect()
}

fn file_count(dir: &Path) -> usize {
    fs::read_dir(dir).unwrap().count()
}

#[test]
fn scan_reports_the_four_categories_and_alias_and_status_agree_without_acting() {
    let f = Fixture::new();
    f.document("A user's guide.md", Some(b"# Changed"), false);
    f.document("gone.md", None, false);
    f.document("invalid.md", Some(b"\xff"), false);
    f.document("archived.md", Some(b"# Changed"), true);
    f.document("unchanged.md", Some(b"# Original"), false);
    f.ok(&["review", "list"]);
    let backups = f.backups("rendered", 21);
    {
        let db = f.db();
        db.execute_batch(
            "INSERT INTO facts(id,key,value,verified_at,kind) VALUES
                (11,'-a quoted key','old',datetime('now','-20 days'),'-tag'),
                (12,'never','old',NULL,NULL),(13,'retired','old',NULL,NULL),
                (14,'superseded','old',NULL,NULL),(15,'fresh','new',CURRENT_TIMESTAMP,NULL);
             UPDATE facts SET retired_at = CURRENT_TIMESTAMP WHERE id = 13;
             UPDATE facts SET superseded_by = 15 WHERE id = 14;
             INSERT INTO tasks(id,title,status,updated_at) VALUES
                (11,'idle open','open','2000-01-01'),(12,'idle blocked','blocked','2000-01-01'),
                (13,'old done','done','2000-01-01'),(14,'old dropped','dropped','2000-01-01'),
                (15,'recent','open',datetime('now','-59 days'));
             INSERT INTO pending_writes(kind,payload_json,rationale,status,actor,actor_raw,created_at) VALUES
                ('fact','{}','expired fixture','expired','fixture','fixture','2000-01-01'),
                ('fact','{}','pending fixture','pending','fixture','fixture','2000-01-01');",
        ).unwrap();
    }

    let db_file = f.repo.join(".memhub/project.sqlite");
    let before = fs::read(&db_file).unwrap();
    let report = f.scan();
    assert_eq!(report["total"], 8);
    for (category, count) in [
        ("stale_fact", 2),
        ("idle_task", 2),
        ("out_of_date_document", 3),
        ("surplus_backups", 1),
    ] {
        assert_eq!(
            report["counts"][category],
            json!({"total":count,"overflow":0})
        );
    }
    assert_eq!(
        f.json(&["review", "stale", "--json"])["review_stale"],
        report
    );
    let scan_human = f.ok(&["prune", "scan"]);
    assert_eq!(scan_human, f.ok(&["review", "stale"]));
    let human = String::from_utf8(scan_human).unwrap();
    for text in [
        "Stale facts",
        "Idle tasks",
        "Out-of-date documents",
        "Surplus backups",
        "memhub fact verify 11",
        "memhub prune backups --yes --candidate",
    ] {
        assert!(human.contains(text), "{human}");
    }
    assert_eq!(
        fs::read(&db_file).unwrap(),
        before,
        "scan and its alias changed the DB"
    );
    assert_eq!(file_count(&backups), 21);
    let status = f.json(&["status", "--json"]);
    assert_eq!(status["status"]["prune_review_count"], report["total"]);
    assert_eq!(status["status"]["stale_queue"], report["total"]);
    assert_eq!(
        status["status"]["pending_writes"], 0,
        "status still runs maintained open"
    );
    assert_eq!(status["status"]["facts"], 5);
    assert!(
        String::from_utf8(f.ok(&["status"]))
            .unwrap()
            .contains("Prune review: 8")
    );

    let fact = row_item(&report, "stale_fact", 11);
    assert_eq!(action(fact, "verify"), ["fact", "verify", "11"]);
    assert_eq!(&action(fact, "retire")[..3], ["fact", "retire", "11"]);
    let mut update = action(fact, "update_value");
    assert_eq!(
        update,
        [
            "fact",
            "add",
            "--kind=-tag",
            "--",
            "-a quoted key",
            "<new-value>"
        ]
    );
    *update.last_mut().unwrap() = "-the updated value";
    f.ok(&update);

    let task = row_item(&report, "idle_task", 11);
    assert_eq!(action(task, "done"), ["task", "done", "11"]);
    assert_eq!(&action(task, "drop")[..3], ["task", "drop", "11"]);
    f.ok(&action(task, "keep"));

    let doc = row_item(&report, "out_of_date_document", 1);
    f.ok(&action(doc, "re_ingest"));
    assert_eq!(
        action(doc, "keep"),
        ["prune", "dismiss", doc["id"].as_str().unwrap()]
    );
    for (id, reason) in [(2, "missing"), (3, "unreadable")] {
        let doc = row_item(&report, "out_of_date_document", id);
        assert!(doc["reason"].as_str().unwrap().contains(reason));
        f.ok(&action(doc, "archive"));
    }
    assert_eq!(f.scan()["total"], 3);
}

#[test]
fn dismiss_is_logged_once_and_source_changes_bring_candidates_back_before_cap() {
    let f = Fixture::new();
    let empty = f.scan();
    assert_eq!(empty["total"], 0);
    assert_eq!(items(&empty).len(), 0);

    f.document("changed.md", Some(b"# Changed"), false);
    let dir = f.backups("rendered", 21);
    f.db()
        .execute_batch(
            "WITH RECURSIVE seq(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM seq WHERE n < 27)
         INSERT INTO facts(id,key,value) SELECT n,'fact ' || n,'old' FROM seq;
         INSERT INTO tasks(id,title,updated_at) VALUES (1,'idle','2000-01-01');",
        )
        .unwrap();
    let before = f.scan();
    assert_eq!(f.scan(), before);
    assert_eq!(before["total"], 30);
    assert_eq!(
        before["counts"]["stale_fact"],
        json!({"total":27,"overflow":2})
    );
    assert_eq!(items(&before).len(), 28);
    let old_ids: Vec<_> = [
        row_item(&before, "stale_fact", 1),
        row_item(&before, "idle_task", 1),
        row_item(&before, "out_of_date_document", 1),
        items(&before).last().unwrap(),
    ]
    .iter()
    .map(|item| item["id"].as_str().unwrap())
    .collect();
    for id in &old_ids {
        f.ok(&["prune", "dismiss", id, "--actor", "fixture:prune"]);
    }
    f.ok(&["prune", "dismiss", old_ids[0], "--actor", "fixture:prune"]);
    assert!(!f.run(&["prune", "dismiss", "made-up"]).status.success());
    let after = f.scan();
    assert_eq!(after["total"], 26);
    assert_eq!(
        after["counts"]["stale_fact"],
        json!({"total":26,"overflow":1})
    );
    assert_eq!(items(&after).len(), 25);
    assert_eq!(row_item(&after, "stale_fact", 26)["subjects"][0]["id"], 26);
    assert_eq!(
        f.json(&["status", "--json"])["status"]["prune_review_count"],
        26
    );
    let log_count: i64 = f
        .db()
        .query_row(
            "SELECT COUNT(*) FROM writes_log w JOIN prune_dismissals p ON w.row_id = p.id
         WHERE w.table_name = 'prune_dismissals' AND w.action = 'insert'
           AND w.actor = 'fixture:prune' AND w.reason = 'prune dismiss ' || p.identifier",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(log_count, 4);

    f.db()
        .execute_batch(
            "UPDATE facts SET value='different' WHERE id=1;
         UPDATE tasks SET notes='different' WHERE id=1;",
        )
        .unwrap();
    fs::write(f.temp.path().join("changed.md"), "# Another change").unwrap();
    // Same mtime and length, different bytes: content participates in the ID.
    let oldest = dir.join("00.bak");
    let original_time = fs::metadata(&oldest).unwrap().modified().unwrap();
    fs::write(&oldest, b"x").unwrap();
    File::options()
        .write(true)
        .open(&oldest)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_time))
        .unwrap();
    let now = f.scan();
    assert_eq!(now["total"], 30);
    for new in [
        row_item(&now, "stale_fact", 1),
        row_item(&now, "idle_task", 1),
        row_item(&now, "out_of_date_document", 1),
        items(&now).last().unwrap(),
    ] {
        assert!(!old_ids.contains(&new["id"].as_str().unwrap()));
    }
}

#[test]
fn scan_reads_0026_without_migrating_and_dismiss_uses_the_standard_migration_backup() {
    let f = Fixture::new();
    f.db()
        .execute_batch(
            "DROP TABLE prune_dismissals;
         DELETE FROM schema_migrations WHERE version = '0027_prune_dismissals';
         UPDATE projects SET schema_version = '0026_memory_lifecycle';
         INSERT INTO facts(id,key,value) VALUES (10,'old','legacy value');",
        )
        .unwrap();
    let before = memhub::commands::sync::LogicalVersion::read(&f.db()).unwrap();
    let slot = f
        .repo
        .join(".memhub/backups/migrations/pre-migration.sqlite");
    let scan = f.scan();
    assert_eq!(scan["total"], 1);
    assert!(!slot.exists());
    assert_eq!(
        memhub::commands::sync::LogicalVersion::read(&f.db()).unwrap(),
        before
    );
    let id = items(&scan)[0]["id"].as_str().unwrap();
    f.ok(&["prune", "dismiss", id]);
    let backup = Connection::open(&slot).unwrap();
    assert_eq!(
        memhub::commands::sync::LogicalVersion::read(&backup).unwrap(),
        before
    );
    let conn = f.db();
    conn.execute(
        "DELETE FROM writes_log WHERE table_name = 'prune_dismissals'",
        [],
    )
    .unwrap();
    let after = memhub::commands::sync::LogicalVersion::read(&conn).unwrap();
    assert_ne!(
        after.digest, before.digest,
        "dismissal itself participates in Drive sync"
    );
    conn.execute(
        "DELETE FROM prune_dismissals WHERE identifier=?1",
        params![id],
    )
    .unwrap();
    assert_eq!(
        memhub::commands::sync::LogicalVersion::read(&conn).unwrap(),
        before
    );
}

#[test]
fn backup_cleanup_requires_confirmation_and_only_removes_reviewed_surplus() {
    let f = Fixture::new();
    let rendered = f.backups("rendered", 23);
    let markdown = f.backups("legacy/nested", 22);
    let safety = f.backups("migrations", 21);
    let unrelated = rendered.join("readme.txt");
    fs::write(&unrelated, "not a backup").unwrap();
    let hardlink = rendered.join("not-a-backup.bak");
    fs::hard_link(&unrelated, &hardlink).unwrap();
    assert!(!f.repo.join("Cargo.toml").exists());
    assert!(!f.run(&["prune", "backups"]).status.success());
    let report = f.scan();
    assert_eq!(report["total"], 2);
    let first = items(&report)
        .iter()
        .find(|item| {
            item["subjects"][0]["directory"]
                .as_str()
                .unwrap()
                .ends_with("rendered")
        })
        .unwrap();
    assert_eq!(first["subjects"][0]["files"].as_array().unwrap().len(), 3);
    let argv = action(first, "delete");
    fs::write(rendered.join("00.bak"), "changed after review").unwrap();
    assert!(!f.run(&argv).status.success());
    assert_eq!(
        file_count(&rendered),
        25,
        "a changed candidate must not delete anything"
    );
    let report = f.scan();
    let first = items(&report)
        .iter()
        .find(|item| {
            item["subjects"][0]["directory"]
                .as_str()
                .unwrap()
                .ends_with("rendered")
        })
        .unwrap();
    let before = fs::read(f.repo.join(".memhub/project.sqlite")).unwrap();
    f.ok(&action(first, "delete"));
    assert_eq!(file_count(&rendered), 22);
    assert_eq!(
        file_count(&markdown),
        22,
        "scoped action must not act on other batches"
    );
    for file in first["subjects"][0]["files"].as_array().unwrap() {
        assert!(!Path::new(file.as_str().unwrap()).exists(), "{file}");
    }
    f.ok(&["prune", "backups", "--yes"]);
    assert_eq!(file_count(&markdown), 20);
    for i in 0..22 {
        assert_eq!(markdown.join(format!("{i:02}.bak")).exists(), i >= 2);
    }
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "not a backup");
    assert!(hardlink.is_file());
    assert_eq!(file_count(&safety), 21);
    assert_eq!(
        fs::read(f.repo.join(".memhub/project.sqlite")).unwrap(),
        before
    );
}

#[test]
fn backup_cleanup_skips_linked_children_and_refuses_a_linked_backup_root() {
    let f = Fixture::new();
    let outside = f.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let victim = outside.join("victim.bak");
    fs::write(&victim, "outside").unwrap();
    for i in 0..21 {
        fs::write(outside.join(format!("{i:02}.bak")), "outside").unwrap();
    }
    let dir = f.backups("rendered", 21);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&victim, dir.join("linked.bak")).unwrap();
    directory_alias(&outside, &dir.join("linked-dir"));
    f.ok(&["prune", "backups", "--yes"]);
    #[cfg(unix)]
    assert!(dir.join("linked.bak").is_symlink());
    assert!(dir.join("linked-dir/victim.bak").is_file());
    let tree = f.repo.join(".memhub/backups");
    fs::rename(&tree, f.repo.join(".memhub/original")).unwrap();
    directory_alias(&outside, &tree);
    assert!(!f.run(&["prune", "backups", "--yes"]).status.success());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "outside");
    assert_eq!(file_count(&outside), 22);
}

#[cfg(unix)]
fn directory_alias(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn directory_alias(target: &Path, link: &Path) {
    // Pass only fixture literals through cmd; the temp path is process cwd.
    let (name, relative_target) = if link.ends_with("linked-dir") {
        ("linked-dir", r"..\..\..\..\outside")
    } else {
        assert!(link.ends_with("backups"));
        ("backups", r"..\..\outside")
    };
    let parent = link.parent().unwrap();
    assert_eq!(
        fs::canonicalize(parent.join(relative_target)).unwrap(),
        fs::canonicalize(target).unwrap()
    );
    let out = Command::new("cmd")
        .current_dir(parent)
        .args(["/C", "mklink", "/J", name, relative_target])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

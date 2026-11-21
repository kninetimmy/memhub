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

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.repo)
            .env("MEMHUB_LOG", "off")
            .env("HOME", self.temp.path())
            .env("USERPROFILE", self.temp.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
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
    ] {
        assert!(human.contains(text), "{human}");
    }
    #[cfg(windows)]
    assert!(
        human.contains("memhub prune backups --yes --candidate"),
        "{human}"
    );
    #[cfg(not(windows))]
    assert!(human.contains("Backup cleanup is report-only"), "{human}");
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
fn scan_and_review_stale_refuse_a_pre_0026_ledger_without_migrating() {
    let f = Fixture::new();
    f.db()
        .execute_batch(
            "DROP TABLE prune_dismissals;
             DELETE FROM schema_migrations WHERE version IN
                 ('0026_memory_lifecycle', '0027_prune_dismissals');
             UPDATE projects SET schema_version = '0025_arch_sections';",
        )
        .unwrap();
    let db_file = f.repo.join(".memhub/project.sqlite");
    let before = fs::read(&db_file).unwrap();
    for args in [["prune", "scan", "--json"], ["review", "stale", "--json"]] {
        let out = f.run(&args);
        assert!(!out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(
            error.contains("cannot read schema 0025_arch_sections"),
            "{error}"
        );
        assert!(error.contains("memhub upgrade"), "{error}");
    }
    assert_eq!(fs::read(db_file).unwrap(), before);
    assert!(
        !f.repo
            .join(".memhub/backups/migrations/pre-migration.sqlite")
            .exists()
    );
}

#[test]
fn scan_uses_missing_config_fallback_without_creating_a_live_config() {
    let f = Fixture::new();
    f.db()
        .execute(
            "INSERT INTO facts(key,value,verified_at) VALUES
             ('aging','value',datetime('now','-20 days'))",
            [],
        )
        .unwrap();
    let live = f.repo.join(".memhub/config.toml");
    let example = f.repo.join(".memhub/config.example.toml");
    fs::rename(&live, &example).unwrap(); // The example has a 10-day horizon.
    let before = fs::read(&example).unwrap();
    let scan = f.scan();
    assert_eq!(scan["total"], 1);
    assert_eq!(f.json(&["review", "stale", "--json"])["review_stale"], scan);
    assert!(!live.exists());
    assert_eq!(fs::read(&example).unwrap(), before);

    fs::write(&live, "broken = [").unwrap();
    assert!(!f.run(&["prune", "scan"]).status.success());
    fs::write(&example, "broken = [").unwrap();
    fs::remove_file(&live).unwrap();
    assert!(!f.run(&["prune", "scan"]).status.success());
    fs::remove_file(&example).unwrap();
    assert_eq!(f.scan()["total"], 0, "built-in horizon applies");
    assert!(!live.exists());
}

#[test]
fn replacing_a_drifted_document_with_identical_bytes_invalidates_its_dismissal() {
    assert_replacement_reopens_dismissal(b"# Changed", "changed since ingest");
}

#[test]
fn replacing_an_invalid_utf8_document_with_identical_bytes_invalidates_its_dismissal() {
    assert_replacement_reopens_dismissal(b"\xff", "unreadable");
}

fn assert_replacement_reopens_dismissal(bytes: &[u8], reason: &str) {
    let f = Fixture::new();
    f.document("changed.md", Some(bytes), false);
    let report = f.scan();
    let previous = row_item(&report, "out_of_date_document", 1);
    assert!(previous["reason"].as_str().unwrap().contains(reason));
    f.ok(&action(previous, "keep"));
    assert_eq!(f.scan()["total"], 0);

    let source = f.temp.path().join("changed.md");
    let replacement = f.temp.path().join("replacement.md");
    fs::copy(&source, &replacement).unwrap();
    File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(fs::metadata(&source).unwrap().modified().unwrap()),
        )
        .unwrap();
    assert_eq!(
        fs::metadata(&source).unwrap().modified().unwrap(),
        fs::metadata(&replacement).unwrap().modified().unwrap()
    );
    fs::rename(&source, f.temp.path().join("old.md")).unwrap();
    fs::rename(&replacement, &source).unwrap();
    let after = f.scan();
    let changed = row_item(&after, "out_of_date_document", 1);
    assert_ne!(previous["id"], changed["id"]);
    assert_eq!(previous["reason"], changed["reason"]);
    assert_eq!(after, f.scan());
}

#[cfg(windows)]
#[test]
fn replacing_a_document_denied_content_access_reopens_its_dismissal() {
    use std::os::windows::fs::OpenOptionsExt;

    let f = Fixture::new();
    f.document("locked.md", Some(b"# Changed"), false);
    let source = f.temp.path().join("locked.md");
    let old = f.temp.path().join("old.md");
    let stamp = fs::metadata(&source).unwrap().modified().unwrap();
    let lock = || {
        File::options()
            .read(true)
            .share_mode(0)
            .open(&source)
            .unwrap()
    };
    let handle = lock();
    let before = f.scan();
    let previous = row_item(&before, "out_of_date_document", 1);
    assert!(previous["reason"].as_str().unwrap().contains("unreadable"));
    f.ok(&action(previous, "keep"));
    assert_eq!(f.scan()["total"], 0);
    drop(handle);

    fs::rename(&source, &old).unwrap();
    fs::copy(&old, &source).unwrap();
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(FileTimes::new().set_modified(stamp))
        .unwrap();
    assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), stamp);
    let _handle = lock();
    let after = f.scan();
    let current = row_item(&after, "out_of_date_document", 1);
    assert_ne!(previous["id"], current["id"]);
    assert_eq!(previous["reason"], current["reason"]);
    assert_eq!(after, f.scan());
    assert_eq!(
        f.json(&["status", "--json"])["status"]["prune_review_count"],
        1
    );
}

#[test]
fn a_dangling_document_link_does_not_match_a_missing_file_dismissal() {
    let f = Fixture::new();
    f.document("gone.md", None, false);
    let report = f.scan();
    let missing = row_item(&report, "out_of_date_document", 1);
    f.ok(&action(missing, "keep"));
    assert_eq!(f.scan()["total"], 0);

    let source = f.temp.path().join("gone.md");
    let target = f.temp.path().join("link-target");
    let create_link = || {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &source).unwrap();
        #[cfg(windows)]
        {
            // Junctions require no symlink privilege and carry reparse metadata.
            let out = Command::new("cmd")
                .current_dir(f.temp.path())
                .args(["/C", "mklink", "/J", "gone.md", "link-target"])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    };
    create_link();
    let stamp = set_link_fixture_mtime(&source);
    assert!(!target.exists());
    let report = f.scan();
    let linked = row_item(&report, "out_of_date_document", 1);
    assert_ne!(missing["id"], linked["id"]);
    assert!(linked["reason"].as_str().unwrap().contains("unreadable"));
    assert_eq!(&action(linked, "archive")[..3], ["doc", "archive", "1"]);

    #[cfg(unix)]
    fs::write(&target, "# Different content").unwrap();
    #[cfg(windows)]
    fs::create_dir(&target).unwrap();
    assert_eq!(
        f.scan(),
        report,
        "the target is not part of the fingerprint"
    );
    f.ok(&action(linked, "keep"));
    f.ok(&action(linked, "keep"));
    assert_eq!(f.scan()["total"], 0);

    fs::rename(&source, f.temp.path().join("old-link")).unwrap();
    create_link();
    assert_eq!(set_link_fixture_mtime(&source), stamp);
    let after = f.scan();
    let replacement = row_item(&after, "out_of_date_document", 1);
    assert_eq!(replacement["reason"], linked["reason"]);
    assert_ne!(replacement["id"], linked["id"]);
    assert_eq!(after, f.scan());
}

fn set_link_fixture_mtime(path: &Path) -> SystemTime {
    #[cfg(unix)]
    {
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let times = [libc::timespec {
            tv_sec: 1_700_000_000,
            tv_nsec: 0,
        }; 2];
        // SAFETY: valid C string and two timestamps; only the fixture link is touched.
        let result = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                name.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        File::options()
            .access_mode(0x100) // FILE_WRITE_ATTRIBUTES
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            )
            .unwrap();
    }
    fs::symlink_metadata(path).unwrap().modified().unwrap()
}

#[cfg(windows)]
#[test]
fn human_action_updates_the_quoted_fact_in_windows_powershell() {
    use std::io::Write;
    use std::process::Stdio;

    const KEY: &str = r#"-a "quoted" key's\"#;
    const PRINTED: &str = r#"& { $PSNativeCommandArgumentPassing = 'Legacy'; memhub fact add -- '"-a \"quoted\" key''s\\"' '<new-value>' }"#;
    // Use a fixed, trusted script matching PRINTED, with only the placeholder
    // replaced. Do not evaluate text read from the program's output.
    const SCRIPT: &str = r#"
if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) { exit 99 }
Set-Alias memhub $env:TEST_MEMHUB_EXE
& { $PSNativeCommandArgumentPassing = 'Legacy'; memhub fact add -- '"-a \"quoted\" key''s\\"' 'updated' }
exit $LASTEXITCODE
"#;
    let f = Fixture::new();
    f.db()
        .execute("INSERT INTO facts(key,value) VALUES (?1,'old')", [KEY])
        .unwrap();
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert_eq!(
        human
            .lines()
            .find_map(|l| l.trim().strip_prefix("update_value: ")),
        Some(PRINTED)
    );

    let mut shell = f
        .command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .env("TEST_MEMHUB_EXE", env!("CARGO_BIN_EXE_memhub"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    shell
        .stdin
        .take()
        .unwrap()
        .write_all(SCRIPT.as_bytes())
        .unwrap();
    let out = shell.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let db = f.db();
    let rows: Vec<(String, String)> = db
        .prepare("SELECT key,value FROM facts")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, [(KEY.into(), "updated".into())]);
}

#[cfg(windows)]
#[test]
fn a_backup_inspection_error_omits_only_the_incomplete_group() {
    use std::os::windows::fs::OpenOptionsExt;

    let f = Fixture::new();
    let incomplete = f.backups("incomplete", 21);
    let nested = f.backups("incomplete/nested", 21);
    let healthy = f.backups("healthy", 21);
    let open = File::options()
        .read(true)
        .share_mode(0)
        .open(incomplete.join("00.bak"))
        .unwrap();
    let scan = f.scan();
    assert_eq!(scan["total"], 2);
    let warning = scan["backup_warning"].as_str().unwrap();
    assert!(
        warning.contains("incomplete") && warning.contains("00.bak"),
        "{warning}"
    );
    let status = f.json(&["status", "--json"]);
    assert_eq!(status["status"]["prune_review_count"], 2);
    assert_eq!(status["status"]["prune_backup_warning"], warning);
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert!(
        human.contains("Surplus backups not counted")
            && human.contains("incomplete")
            && human.contains("00.bak"),
        "{human}"
    );

    let healthy_batch = &items(&scan)[0];
    assert!(
        healthy_batch["subjects"][0]["directory"]
            .as_str()
            .unwrap()
            .ends_with("healthy")
    );
    f.ok(&action(healthy_batch, "delete"));
    assert_eq!(file_count(&healthy), 20);
    assert_eq!(file_count(&nested), 21);
    assert_eq!(file_count(&incomplete), 22);
    drop(open);
    assert_eq!(
        f.scan()["total"],
        2,
        "both remaining batches are readable again"
    );
}

#[cfg(windows)]
#[test]
fn scan_and_cleanup_reject_known_readonly_or_alternate_stream_targets() {
    let f = Fixture::new();
    let readonly = f.backups("readonly", 22).join("00.bak");
    let readonly_meta = fs::metadata(&readonly).unwrap();
    let mut permissions = readonly_meta.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly, permissions).unwrap();
    let streamed = f.backups("streamed", 22).join("00.bak");
    let modified = fs::metadata(&streamed).unwrap().modified().unwrap();
    let mut stream = streamed.as_os_str().to_os_string();
    stream.push(":notes");
    fs::write(&stream, "additional content").unwrap();
    File::options()
        .write(true)
        .open(&streamed)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();

    let scan = f.scan();
    assert_eq!(scan["total"], 2);
    for (item, reason) in items(&scan).iter().zip(["read-only", "unreviewed streams"]) {
        assert!(item["reason"].as_str().unwrap().contains(reason), "{item}");
        assert_eq!(item["actions"].as_array().unwrap().len(), 1);
        let id = item["id"].as_str().unwrap();
        assert_eq!(action(item, "keep"), ["prune", "dismiss", id]);
        let out = f.run(&["prune", "backups", "--yes", "--candidate", id]);
        assert!(!out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(reason),
            "{out:?}"
        );
    }
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert!(human.contains("read-only") && human.contains("unreviewed streams"));
    assert!(!human.contains("memhub prune backups --yes --candidate"));
    assert_eq!(file_count(readonly.parent().unwrap()), 22);
    assert_eq!(file_count(streamed.parent().unwrap()), 22);
    assert_eq!(fs::read_to_string(&stream).unwrap(), "additional content");

    let blocked = &items(&scan)[0];
    assert_eq!(f.scan(), scan);
    f.ok(&action(blocked, "keep"));
    f.ok(&action(blocked, "keep"));
    assert_eq!(items(&f.scan()), &items(&scan)[1..]);
    fs::set_permissions(&readonly, readonly_meta.permissions()).unwrap();
    let after = f.scan();
    assert_eq!(after["total"], 2);
    let restored = &items(&after)[0];
    assert_eq!(restored["subjects"], blocked["subjects"]);
    assert_ne!(restored["id"], blocked["id"]);
    assert_eq!(
        action(restored, "delete").last().copied(),
        restored["id"].as_str()
    );
    assert_eq!(
        fs::metadata(&readonly).unwrap().modified().unwrap(),
        readonly_meta.modified().unwrap()
    );
    assert_eq!(fs::read(&readonly).unwrap(), b"a");
    let old_dismissal = f.run(&action(blocked, "keep"));
    assert!(!old_dismissal.status.success());
    assert!(String::from_utf8_lossy(&old_dismissal.stderr).contains("no current prune candidate"));
    assert_eq!(&items(&after)[1], &items(&scan)[1]);
    assert_eq!(f.scan(), after);

    let blocked_stream = &items(&after)[1];
    f.ok(&action(blocked_stream, "keep"));
    assert_eq!(f.scan()["total"], 1);
    fs::remove_file(&stream).unwrap();
    File::options()
        .write(true)
        .open(&streamed)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    let after = f.scan();
    assert_eq!(after["total"], 2);
    let restored = &items(&after)[1];
    assert_eq!(restored["subjects"], blocked_stream["subjects"]);
    assert_ne!(restored["id"], blocked_stream["id"]);
    assert_eq!(
        action(restored, "delete").last().copied(),
        restored["id"].as_str()
    );
}

#[test]
fn backup_safety_slots_ignore_ascii_case_on_all_platforms() {
    let f = Fixture::new();
    for name in ["SyNc", "ImPoRt", "MiGrAtIoNs"] {
        f.backups(name, 21);
    }
    f.backups("ordinary/SyNc", 21);
    let report = f.scan();
    assert_eq!(report["total"], 1);
    let suffix = format!("ordinary{}SyNc", std::path::MAIN_SEPARATOR);
    assert!(
        items(&report)[0]["subjects"][0]["directory"]
            .as_str()
            .unwrap()
            .ends_with(&suffix)
    );
}

// APFS does not allow creating the invalid UTF-8 entry used by this fixture.
#[cfg(target_os = "linux")]
#[test]
fn scan_distinguishes_invalid_utf8_from_literal_escape_and_replacement_character() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let f = Fixture::new();
    let dir = f.backups("rendered", 23);
    for (i, name) in [b"\xff.bak".as_slice(), br"\xff.bak", "�.bak".as_bytes()]
        .into_iter()
        .enumerate()
    {
        fs::rename(
            dir.join(format!("{i:02}.bak")),
            dir.join(OsStr::from_bytes(name)),
        )
        .unwrap();
    }
    let scan = f.scan();
    let files = items(&scan)[0]["subjects"][0]["files"].as_array().unwrap();
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert_eq!(files.len(), 3);
    for name in [r"/\xff.bak", r"/\\xff.bak", "/�.bak"] {
        let displayed = files
            .iter()
            .find_map(|file| file.as_str().filter(|path| path.ends_with(name)))
            .unwrap_or_else(|| panic!("no distinct path ending in {name:?}: {files:?}"));
        assert!(human.contains(&format!("{displayed:?}")), "{human}");
    }
    assert_eq!(file_count(&dir), 23);
}

#[cfg(windows)]
#[test]
fn scan_distinguishes_unpaired_utf16_from_literal_escape_and_replacement_character() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let f = Fixture::new();
    let dir = f.backups("rendered", 23);
    let mut unpaired = vec![0xd800];
    unpaired.extend(".bak".encode_utf16());
    for (i, name) in [
        OsString::from_wide(&unpaired),
        "%ud800.bak".into(),
        "�.bak".into(),
    ]
    .into_iter()
    .enumerate()
    {
        fs::rename(dir.join(format!("{i:02}.bak")), dir.join(name)).unwrap();
    }
    let scan = f.scan();
    let files = items(&scan)[0]["subjects"][0]["files"].as_array().unwrap();
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert_eq!(files.len(), 3);
    for name in [r"\%ud800.bak", r"\%%ud800.bak", r"\�.bak"] {
        let displayed = files
            .iter()
            .find_map(|file| file.as_str().filter(|path| path.ends_with(name)))
            .unwrap_or_else(|| panic!("no distinct path ending in {name:?}: {files:?}"));
        assert!(human.contains(&format!("{displayed:?}")), "{human}");
    }
    assert_eq!(file_count(&dir), 23);
}

#[cfg(windows)]
#[test]
fn backup_cleanup_requires_confirmation_and_only_removes_reviewed_surplus() {
    let f = Fixture::new();
    let rendered = f.backups("rendered", 23);
    let markdown = f.backups("legacy/nested", 22);
    let oldest = markdown.join("00.bak");
    let pre_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    File::options()
        .write(true)
        .open(&oldest)
        .unwrap()
        .set_times(FileTimes::new().set_modified(pre_epoch))
        .unwrap();
    assert_eq!(
        fs::metadata(&oldest).unwrap().modified().unwrap(),
        pre_epoch
    );
    let safety = f.backups("MiGrAtIoNs", 21);
    let unrelated = rendered.join("readme.txt");
    fs::write(&unrelated, "not a backup").unwrap();
    let hardlink = rendered.join("not-a-backup.bak");
    fs::hard_link(&unrelated, &hardlink).unwrap();
    assert!(!f.repo.join("Cargo.toml").exists());
    assert!(!f.run(&["prune", "backups"]).status.success());
    let report = f.scan();
    assert_eq!(report["total"], 2);
    assert_eq!(
        f.json(&["status", "--json"])["status"]["prune_review_count"],
        2
    );
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
    assert!(!f.run(&["prune", "backups", "--yes"]).status.success());
    assert!(
        !f.run(&["prune", "backups", "--candidate", argv[4]])
            .status
            .success()
    );
    assert_eq!(file_count(&rendered), 25);
    assert_eq!(file_count(&markdown), 22);
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
        let name = Path::new(file.as_str().unwrap()).file_name().unwrap();
        assert!(!rendered.join(name).exists(), "{file}");
    }
    let report = f.scan();
    assert_eq!(report["total"], 1);
    f.ok(&action(&items(&report)[0], "delete"));
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

#[cfg(windows)]
#[test]
fn backup_cleanup_refuses_before_any_deletion_when_a_keeper_or_victim_is_busy() {
    use std::os::windows::fs::OpenOptionsExt;

    let f = Fixture::new();
    let dir = f.backups("rendered", 22);
    let report = f.scan();
    let delete = action(&items(&report)[0], "delete");
    for name in ["21.bak", "00.bak"] {
        let busy = File::options()
            .write(true)
            .share_mode(7)
            .open(dir.join(name))
            .unwrap();
        let out = f.run(&delete);
        assert!(!out.status.success(), "{out:?}");
        assert_eq!(file_count(&dir), 22);
        drop(busy);
    }
    assert_eq!(f.scan(), report);
    f.ok(&delete);
    assert_eq!(file_count(&dir), 20);
}

#[cfg(windows)]
#[test]
fn backup_cleanup_with_an_attribute_reader_is_complete_or_refused() {
    use std::os::windows::fs::OpenOptionsExt;

    let f = Fixture::new();
    let dir = f.backups("rendered", 22);
    let report = f.scan();
    let delete = action(&items(&report)[0], "delete");
    let target = dir.join("00.bak");
    let reader = File::options()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .share_mode(7)
        .open(&target)
        .unwrap();
    let out = f.run(&delete);
    let namespace = || {
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let held = namespace();
    drop(reader);
    let released = namespace();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    eprintln!(
        "attribute reader: exit={}, stdout={stdout:?}, stderr={stderr:?}, held={held:?}, released={released:?}",
        out.status
    );
    assert_eq!(
        held, released,
        "cleanup must not be deferred past the command"
    );
    let expected: Vec<_> = (if out.status.success() { 2 } else { 0 }..22)
        .map(|i| format!("{i:02}.bak"))
        .collect();
    assert_eq!(held, expected);
    if out.status.success() {
        assert_eq!(stdout.trim(), "Deleted 2 surplus backup file(s)");
    } else {
        assert!(stdout.is_empty(), "{stdout}");
        let stdout = String::from_utf8(f.ok(&delete)).unwrap();
        let after = namespace();
        eprintln!("after retry: stdout={stdout:?}, names={after:?}");
        assert_eq!(stdout.trim(), "Deleted 2 surplus backup file(s)");
        assert_eq!(
            after,
            (2..22).map(|i| format!("{i:02}.bak")).collect::<Vec<_>>()
        );
    }
}

#[cfg(windows)]
#[test]
fn backup_cleanup_requires_a_new_review_after_an_identical_file_replacement() {
    let f = Fixture::new();
    let dir = f.backups("rendered", 21);
    let report = f.scan();
    let delete = action(&items(&report)[0], "delete");
    let oldest = dir.join("00.bak");
    let stamp = fs::metadata(&oldest).unwrap().modified().unwrap();
    let replacement = f.temp.path().join("replacement");
    fs::copy(&oldest, &replacement).unwrap();
    fs::remove_file(&oldest).unwrap();
    fs::rename(&replacement, &oldest).unwrap();
    File::options()
        .write(true)
        .open(&oldest)
        .unwrap()
        .set_times(FileTimes::new().set_modified(stamp))
        .unwrap();
    assert!(!f.run(&delete).status.success());
    assert_eq!(file_count(&dir), 21);
    assert_ne!(items(&report)[0]["id"], items(&f.scan())[0]["id"]);
}

#[cfg(not(windows))]
#[test]
fn backup_cleanup_reports_surplus_but_refuses_direct_invocations() {
    let f = Fixture::new();
    let dir = f.backups("rendered", 22);
    let report = f.scan();
    let item = &items(&report)[0];
    assert_eq!(item["subjects"][0]["files"].as_array().unwrap().len(), 2);
    assert_eq!(item["actions"].as_array().unwrap().len(), 1);
    assert!(item["reason"].as_str().unwrap().contains("report-only"));
    let id = item["id"].as_str().unwrap();
    assert_eq!(action(item, "keep"), ["prune", "dismiss", id]);
    for argv in [
        &["prune", "backups"][..],
        &["prune", "backups", "--yes"][..],
        &["prune", "backups", "--candidate", id],
        &["prune", "backups", "--yes", "--candidate", id],
    ] {
        let out = f.run(argv);
        assert!(!out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("report-only"));
    }
    assert_eq!(f.scan(), report);
    assert_eq!(file_count(&dir), 22);
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
    let initial = f.scan();
    let item = &items(&initial)[0];
    let id = item["id"].as_str().unwrap();
    let argv = ["prune", "backups", "--yes", "--candidate", id];
    #[cfg(windows)]
    f.ok(&argv);
    #[cfg(not(windows))]
    assert!(!f.run(&argv).status.success());
    #[cfg(unix)]
    assert!(dir.join("linked.bak").is_symlink());
    assert!(dir.join("linked-dir/victim.bak").is_file());
    let tree = f.repo.join(".memhub/backups");
    fs::rename(&tree, f.repo.join(".memhub/original")).unwrap();
    directory_alias(&outside, &tree);
    f.db()
        .execute("INSERT INTO facts(key,value) VALUES ('aging','value')", [])
        .unwrap();
    let report = f.scan();
    assert_eq!(report["total"], 1);
    assert_eq!(report["counts"]["surplus_backups"]["total"], 0);
    assert_eq!(
        row_item(&report, "stale_fact", 1)["subjects"][0]["key"],
        "aging"
    );
    assert_eq!(
        f.json(&["review", "stale", "--json"])["review_stale"],
        report
    );
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert!(human.contains("Surplus backups not counted"));
    let status = f.json(&["status", "--json"]);
    assert_eq!(status["status"]["prune_review_count"], report["total"]);
    assert_eq!(
        status["status"]["prune_backup_warning"],
        report["backup_warning"]
    );
    let human = String::from_utf8(f.ok(&["status"])).unwrap();
    assert!(human.contains("Surplus backups not counted"));
    let id = row_item(&report, "stale_fact", 1)["id"].as_str().unwrap();
    f.ok(&["prune", "dismiss", id]);
    assert!(!f.run(&argv).status.success());
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

use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const APPLY: &[&str] = &["prune", "apply", "--request", TOKEN, "--host", "claude"];

fn scan(f: &Fixture) -> Value {
    f.json(&["prune", "scan", "--for-wrap-up", "--json"])["prune_wrap_up"].clone()
}

fn request(report: &Value, item: &Value, action: &str) -> Value {
    json!({
        "version": report["version"],
        "project": report["project"],
        "candidate": item["id"],
        "target": item["offer"]["target"],
        "action": action,
    })
}

fn file(f: &Fixture) -> PathBuf {
    f.repo.join(format!(".memhub/prune-apply-{TOKEN}.json"))
}

fn write(f: &Fixture, request: &Value) {
    fs::write(file(f), serde_json::to_vec(request).unwrap()).unwrap();
}

fn refuse(f: &Fixture, request: &Value, error: &str) {
    write(f, request);
    let out = f.run(APPLY);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains(error), "{out:?}");
}

#[test]
fn wrapup_apply_stages_exact_stock_shell_data_then_requires_separate_acceptance() {
    let f = Fixture::new();
    let key = "--key with spaces, quotes and $";
    let replacement = "  line one: \"quote\" 'single' \u{03bb} & | ; `$x` $(text) \\ \nline two";
    f.db()
        .execute(
            "INSERT INTO facts(key,value,kind) VALUES (?1,'old','constraint')",
            [key],
        )
        .unwrap();
    let report = scan(&f);
    let item = row_item(&report, "stale_fact", 1);
    assert_eq!(item["id"], row_item(&f.scan(), "stale_fact", 1)["id"]);
    assert!(
        item.get("actions").is_none(),
        "no manual argv in wrap-up output: {item}"
    );
    assert_eq!(item["offer"]["actions"], json!(["verify", "update_value"]));
    refuse(&f, &request(&report, item, "retire"), "unknown variant");
    refuse(
        &f,
        &request(&report, item, "update_value"),
        "invalid prune request",
    );

    let mut reviewed = request(&report, item, "update_value");
    reviewed["replacement"] = json!(replacement);
    write(&f, &reviewed);
    // The script is fixed; neither the reviewed data nor the binary path is shell source.
    #[cfg(windows)]
    let (shell, args, script) = (
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command"][..],
        r#"Set-Alias memhub $env:TEST_MEMHUB_EXE; memhub prune apply --request 0123456789abcdef0123456789abcdef --host claude; exit $LASTEXITCODE"#,
    );
    #[cfg(not(windows))]
    let (shell, args, script) = (
        "sh",
        &["-c"][..],
        r#""$TEST_MEMHUB_EXE" prune apply --request 0123456789abcdef0123456789abcdef --host claude"#,
    );
    let output = f
        .command(shell)
        .args(args)
        .arg(script)
        .env("TEST_MEMHUB_EXE", env!("CARGO_BIN_EXE_memhub"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = result["prune_apply"]["pending_id"].as_i64().unwrap();
    let db = f.db();
    let staged: (String, String, String, String, String) = db
        .query_row(
            "SELECT p.status, p.payload_json, p.actor, p.actor_raw, w.actor
         FROM pending_writes p JOIN writes_log w ON w.row_id=p.id
         WHERE p.id=?1 AND w.table_name='pending_writes'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(staged.0, "pending");
    assert_eq!(
        serde_json::from_str::<Value>(&staged.1).unwrap(),
        json!({
            "key": key, "value": replacement, "kind": "constraint"
        })
    );
    assert_eq!(staged.2, "claude-code");
    assert_eq!(staged.3, "claude:wrap-up");
    assert_eq!(staged.4, "claude:wrap-up");
    let durable = || {
        db.query_row("SELECT value, source FROM facts WHERE id=1", [], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
    };
    assert_eq!(durable().0, "old");
    f.ok(&["review", "accept", &id.to_string(), "--force"]);
    assert_eq!(
        durable(),
        (replacement.to_owned(), "user+agent:claude-code".to_owned())
    );
}

#[test]
fn wrapup_apply_refuses_wrong_project_row_action_and_request_shape() {
    let f = Fixture::new();
    let other = Fixture::new();
    for fixture in [&f, &other] {
        fixture
            .db()
            .execute_batch(
                "INSERT INTO tasks(title,status,created_at,updated_at) VALUES
                ('first','open','2000-01-01','2000-01-01'),
                ('second','blocked','2000-01-01','2000-01-01'),
                ('third','open','2000-01-01','2000-01-01');",
            )
            .unwrap();
    }
    let report = scan(&f);
    let item = row_item(&report, "idle_task", 1);
    let reviewed = request(&report, item, "drop");
    assert_eq!(item["id"], row_item(&scan(&other), "idle_task", 1)["id"]);
    refuse(&other, &reviewed, "does not match this local project");
    let mut wrong_row = reviewed.clone();
    wrong_row["target"]["id"] = json!(2);
    refuse(&f, &wrong_row, "does not offer this action");
    refuse(
        &f,
        &request(&report, item, "verify"),
        "does not offer this action",
    );
    let mut path = reviewed.clone();
    path["target"]["path"] = json!("../unrelated");
    refuse(&f, &path, "unknown field");
    let mut command = reviewed.clone();
    command["command"] = json!(["other-program"]);
    refuse(&f, &command, "unknown field");
    refuse(&f, &json!([reviewed, reviewed]), "must be one JSON object");
    write(&f, &reviewed);
    assert!(
        !f.run(&[
            "prune",
            "apply",
            "--request",
            "../request",
            "--host",
            "claude"
        ])
        .status
        .success()
    );
    let mut args = APPLY.to_vec();
    args.extend(["--request", TOKEN]);
    assert!(!f.run(&args).status.success());
    let child = f.repo.join("child");
    fs::create_dir(&child).unwrap();
    assert!(
        !f.command(env!("CARGO_BIN_EXE_memhub"))
            .current_dir(&child)
            .args(APPLY)
            .output()
            .unwrap()
            .status
            .success()
    );
    let db = f.db();
    db.execute("UPDATE tasks SET notes='changed' WHERE id=1", [])
        .unwrap();
    refuse(&f, &reviewed, "missing, changed");
    assert_eq!(
        db.query_row("SELECT status FROM tasks WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "open"
    );
    let report = scan(&f);
    for (id, action) in [(1, "drop"), (2, "keep"), (3, "done")] {
        write(
            &f,
            &request(&report, row_item(&report, "idle_task", id), action),
        );
        assert_eq!(f.json(APPLY)["prune_apply"]["action"], action);
    }
    assert!(items(&scan(&f)).is_empty());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM writes_log WHERE table_name='tasks' AND actor='claude:wrap-up'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
}

#[test]
fn wrapup_apply_checks_source_and_row_then_updates_existing_document() {
    let f = Fixture::new();
    f.document("guide.md", Some(b"# Reviewed content"), false);
    f.document("missing.md", None, false);
    let path = f.temp.path().join("guide.md");
    let missing = f.temp.path().join("missing.md");
    let report = scan(&f);
    let ingest = request(
        &report,
        row_item(&report, "out_of_date_document", 1),
        "re_ingest",
    );
    let archive = request(
        &report,
        row_item(&report, "out_of_date_document", 2),
        "archive",
    );
    fs::write(&path, "# Unreviewed edit").unwrap();
    refuse(&f, &ingest, "missing, changed");
    fs::write(&missing, "# Restored").unwrap();
    refuse(&f, &archive, "missing, changed");
    fs::write(&path, "# Reviewed content").unwrap();
    fs::remove_file(&missing).unwrap();
    let db = f.db();
    db.execute(
        "UPDATE documents SET title='New stored title' WHERE id=1",
        [],
    )
    .unwrap();
    refuse(&f, &ingest, "missing, changed");
    let refreshed = scan(&f);
    write(
        &f,
        &request(
            &refreshed,
            row_item(&refreshed, "out_of_date_document", 1),
            "re_ingest",
        ),
    );
    f.ok(APPLY);
    write(&f, &archive);
    f.ok(APPLY);
    assert_eq!(
        db.query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT title FROM documents WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "New stored title"
    );
    assert_eq!(
        db.query_row("SELECT body FROM doc_chunks WHERE doc_id=1", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "# Reviewed content"
    );
    assert!(
        db.query_row(
            "SELECT archived_at IS NOT NULL FROM documents WHERE id=2",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[test]
fn wrapup_apply_refuses_a_copied_database_at_the_same_project_path() {
    let f = Fixture::new();
    f.db()
        .execute(
            "INSERT INTO tasks(title,updated_at) VALUES ('idle','2000-01-01')",
            [],
        )
        .unwrap();
    let report = scan(&f);
    let reviewed = request(&report, row_item(&report, "idle_task", 1), "done");
    let original = f.repo.join(".memhub/project.sqlite");
    let parked = f.repo.join(".memhub/parked.sqlite");
    fs::rename(&original, &parked).unwrap();
    fs::copy(&parked, &original).unwrap();
    let copy = scan(&f);
    assert_eq!(row_item(&copy, "idle_task", 1)["id"], reviewed["candidate"]);
    refuse(&f, &reviewed, "does not match this local project/database");
    assert_eq!(
        row_item(&scan(&f), "idle_task", 1)["id"],
        reviewed["candidate"]
    );
}

#[test]
fn wrapup_apply_dismisses_only_the_selected_pair_and_does_not_rerun_the_scan() {
    let f = Fixture::new();
    let path = f.repo.join(".memhub/config.toml");
    let mut config = memhub::config::ProjectConfig::load(&path).unwrap();
    config.retrieval.use_reranker = true;
    config.save(&path).unwrap();
    let db = f.db();
    db.execute_batch(
        "INSERT INTO facts(key,value) VALUES
         ('datastore','We will run a Postgres server to hold project memory.'),
         ('database','We will run a Postgres server to hold project memory.'), ('other','Cats are household pets.');"
    ).unwrap();
    let report = scan(&f);
    let item = items(&report)
        .iter()
        .find(|i| i["category"] == "duplicate_facts")
        .unwrap();
    let reviewed = request(&report, item, "dismiss");
    let mut wrong_pair = reviewed.clone();
    wrong_pair["target"]["second"]["id"] = json!(3);
    // An accidental broad rescan would fail on the missing decisions table.
    db.execute_batch("DROP TABLE decisions;").unwrap();
    refuse(&f, &wrong_pair, "missing, changed");
    write(&f, &reviewed);
    f.ok(APPLY);
    refuse(&f, &reviewed, "dismissed");
    let fact = request(&report, row_item(&report, "stale_fact", 1), "verify");
    write(&f, &fact);
    f.ok(APPLY);
    assert!(
        db.query_row(
            "SELECT verified_at IS NOT NULL FROM facts WHERE id=1",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[test]
fn wrapup_apply_reuses_exact_backup_groups_and_preserves_safety_slots() {
    let f = Fixture::new();
    let keep = f.backups("sync", 25);
    let surplus = f.backups("ordinary", 22);
    let report = scan(&f);
    let [item] = items(&report) else {
        panic!("{report}");
    };
    assert_eq!(item["category"], "surplus_backups");
    let supported = item["offer"]["actions"]
        .as_array()
        .unwrap()
        .contains(&json!("delete"));
    let deletion = request(&report, item, "delete");
    if supported {
        fs::write(surplus.join("00.bak"), b"changed").unwrap();
        refuse(&f, &deletion, "no current surplus backup candidate");
        let current = scan(&f);
        write(&f, &request(&current, &items(&current)[0], "delete"));
        assert_eq!(f.json(APPLY)["prune_apply"]["removed"], 2);
        assert_eq!(file_count(&surplus), 20);
    } else {
        assert!(
            !cfg!(windows) || std::env::var_os("MEMHUB_TEST_PRUNE_ROOT").is_none(),
            "{item}"
        );
        refuse(&f, &deletion, "does not offer this action");
        assert_eq!(file_count(&surplus), 22);
    }
    assert_eq!(file_count(&keep), 25);
}

#[test]
fn wrapup_apply_does_not_delete_a_dismissed_backup_group() {
    let f = Fixture::new();
    let surplus = f.backups("ordinary", 22);
    let report = scan(&f);
    let item = &items(&report)[0];
    write(&f, &request(&report, item, "keep"));
    f.ok(APPLY);
    assert!(items(&scan(&f)).is_empty());
    refuse(&f, &request(&report, item, "delete"), "dismissed");
    assert_eq!(file_count(&surplus), 22);
}

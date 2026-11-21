use super::*;

const CLAIM: &str = "We will run a Postgres server to hold project memory.";

fn fixture() -> Fixture {
    let f = Fixture::new();
    let path = f.repo.join(".memhub/config.toml");
    let mut cfg = memhub::config::ProjectConfig::load(&path).unwrap();
    cfg.retrieval.use_reranker = true;
    cfg.save(&path).unwrap();
    f
}

fn category<'a>(report: &'a Value, name: &str) -> Vec<&'a Value> {
    items(report)
        .iter()
        .filter(|i| i["category"] == name)
        .collect()
}

#[test]
fn similarity_categories_filter_sources_and_preserve_exact_paired_excerpts_without_writes() {
    let f = fixture();
    let doc = f.repo.join("guide.md");
    let body = format!(
        "{CLAIM}\n\n{}\n\nThis applies to version 1.0 only, not version 2.0.",
        "The datastore holds project memory. ".repeat(10)
    );
    fs::write(&doc, &body).unwrap();
    f.ok(&["doc", "add", "--title=Guide", "--", doc.to_str().unwrap()]);
    let recorded_doc = fs::canonicalize(&doc).unwrap();
    f.ok(&["arch", "set", "--", &body]);
    let backups = f.backups("rendered", 21);
    let db = f.db();
    for (id, key) in [
        (1, "datastore"),
        (2, "database"),
        (3, "retired"),
        (4, "superseded"),
        (5, "chain"),
    ] {
        db.execute(
            "INSERT INTO facts(id,key,value,verified_at) VALUES (?1,?2,?3,CURRENT_TIMESTAMP)",
            params![id, key, CLAIM],
        )
        .unwrap();
    }
    db.execute_batch(
        "UPDATE facts SET created_at = '2020-01-01', retired_at = '2020-01-01', retired_reason = 'old' WHERE id = 3;
         UPDATE facts SET superseded_by = 1 WHERE id = 4;
         UPDATE facts SET superseded_by = 4 WHERE id = 5;
         INSERT INTO facts(id,key,value,verified_at)
         VALUES (6,'favorite animal','Cats are domesticated felines kept as household pets.',CURRENT_TIMESTAMP);",
    )
    .unwrap();
    // Recorded date, not insertion order, determines earlier/later.
    for (id, date, status, replacement) in [
        (1, "2025-01-01", "active", None),
        (2, "2020-01-01", "active", None),
        (3, "2019-01-01", "superseded", Some(1)),
        (4, "2018-01-01", "draft", None),
        (5, "2017-01-01", "active", Some(1)),
        (6, "2016-01-01", "superseded", None),
    ] {
        db.execute(
            "INSERT INTO decisions(id,title,rationale,summary,decided_at,status,superseded_by)
             VALUES (?1,'Adopt Postgres as the datastore',?2,'Version 1.0 only',?3,?4,?5)",
            params![id, CLAIM, date, status, replacement],
        )
        .unwrap();
    }
    for (id, status) in [(1, "open"), (2, "blocked"), (3, "done"), (4, "dropped")] {
        db.execute(
            "INSERT INTO tasks(id,title,notes,status) VALUES (?1,?2,'Version 1.0 only',?3)",
            params![id, CLAIM, status],
        )
        .unwrap();
    }
    db.execute_batch(
        "INSERT INTO tasks(id,title) VALUES (5,'Cats are domesticated felines kept as household pets.');",
    ).unwrap();
    db.execute(
        "INSERT INTO session_notes(text,actor,actor_raw) VALUES (?1,'fixture','fixture')",
        [CLAIM],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO documents(id,path,title,content_hash,byte_len,archived_at)
         VALUES (99,'archived.md','Archived','hash',1,CURRENT_TIMESTAMP);
         INSERT INTO doc_chunks(id,doc_id,ord,heading_path,body)
         VALUES (99,99,0,'Postgres','We will run a Postgres server to hold project memory.');",
    )
    .unwrap();
    drop(db);
    let db_path = f.repo.join(".memhub/project.sqlite");
    let before = fs::read(&db_path).unwrap();
    let report = f.scan();
    assert_eq!(f.scan(), report);
    assert_eq!(
        f.json(&["review", "stale", "--json"])["review_stale"],
        report
    );
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert_eq!(fs::read_to_string(&doc).unwrap(), body);
    assert_eq!(file_count(&backups), 21);
    assert!(report.get("similarity_warning").is_none());
    let duplicates = category(&report, "duplicate_facts");
    assert_eq!(duplicates.len(), 1);
    assert_eq!(
        &action(duplicates[0], "retire_earlier")[..3],
        ["fact", "retire", "1"]
    );
    assert_eq!(
        &action(duplicates[0], "retire_later")[..3],
        ["fact", "retire", "2"]
    );
    let decisions = category(&report, "unlinked_decisions");
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        action(decisions[0], "supersede_earlier"),
        ["decision", "supersede", "2", "--by", "1"]
    );
    let sweep = category(&report, "correction_sweep");
    for old in [
        ("fact", 3),
        ("fact", 4),
        ("fact", 5),
        ("decision", 3),
        ("decision", 5),
        ("decision", 6),
    ] {
        assert!(
            sweep
                .iter()
                .any(|item| item["excerpts"]["old_claim"]["source_type"] == old.0
                    && item["excerpts"]["old_claim"]["source_id"] == old.1),
            "{old:?}: {report}"
        );
    }
    for item in &sweep {
        let old = &item["excerpts"]["old_claim"];
        let target = &item["excerpts"]["possible_restatement"];
        assert!(old["recorded_at"].is_string());
        assert!(target["recorded_at"].is_string());
        assert!(old["text"].as_str().unwrap().contains(CLAIM));
        assert!(target["text"].as_str().unwrap().contains(CLAIM));
        assert!(matches!(
            target["source_type"].as_str().unwrap(),
            "fact" | "arch_section" | "doc_chunk" | "task"
        ));
        if target["source_type"] == "fact" {
            assert!(target["source_id"] == 1 || target["source_id"] == 2);
        } else {
            assert_eq!(target["source_id"], 1);
        }
        if old["source_type"] == "decision" {
            assert_ne!(old["source_id"], 4, "draft decisions are not old claims");
            assert_eq!(
                old["text"],
                format!("Version 1.0 only\n\nAdopt Postgres as the datastore\n\n{CLAIM}")
            );
        }
    }
    // Check chain exclusion by ID (other active fact #2 must still be offered).
    for id in [4, 5] {
        let targets: Vec<_> = sweep
            .iter()
            .filter(|item| {
                item["excerpts"]["old_claim"]["source_type"] == "fact"
                    && item["excerpts"]["old_claim"]["source_id"] == id
            })
            .collect();
        assert!(
            targets.iter().any(
                |i| i["excerpts"]["possible_restatement"]["source_type"] == "fact"
                    && i["excerpts"]["possible_restatement"]["source_id"] == 2
            )
        );
        assert!(
            !targets.iter().any(
                |i| i["excerpts"]["possible_restatement"]["source_type"] == "fact"
                    && i["excerpts"]["possible_restatement"]["source_id"] == 1
            )
        );
    }
    for (kind, expected_text) in [
        ("fact", format!("datastore: {CLAIM}")),
        ("doc_chunk", body.clone()),
        ("arch_section", body.clone()),
        ("task", format!("{CLAIM}\n\nVersion 1.0 only")),
    ] {
        let hit = sweep
            .iter()
            .find(|i| i["excerpts"]["possible_restatement"]["source_type"] == kind)
            .unwrap();
        assert_eq!(
            hit["excerpts"]["possible_restatement"]["text"],
            expected_text
        );
        match kind {
            "fact" => assert_eq!(&action(hit, "retire")[..2], ["fact", "retire"]),
            "doc_chunk" => assert_eq!(
                action(hit, "edit_and_re_ingest"),
                [
                    "doc",
                    "add",
                    "--title=Guide",
                    "--",
                    recorded_doc.to_str().unwrap()
                ]
            ),
            "arch_section" => assert_eq!(
                action(hit, "rewrite"),
                ["arch", "set", "--", "<complete-rewritten-architecture>"]
            ),
            "task" => {
                assert_eq!(&action(hit, "drop")[..3], ["task", "drop", "1"]);
                assert_eq!(action(hit, "keep_task"), ["task", "keep", "1"]);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            action(hit, "keep"),
            ["prune", "dismiss", hit["id"].as_str().unwrap()]
        );
    }
    assert!(
        !sweep
            .iter()
            .any(|i| i["excerpts"]["possible_restatement"]["source_id"] == 99)
    );
    for text in [
        "old claim",
        "possible restatement",
        "Version 1.0 only",
        "version 2.0",
        "recorded 2020-01-01",
    ] {
        assert!(human.contains(text), "missing {text:?} in human output");
    }
    let status = f.json(&["status", "--json"]);
    let similarity_total: u64 = ["duplicate_facts", "unlinked_decisions", "correction_sweep"]
        .iter()
        .map(|key| report["counts"][key]["total"].as_u64().unwrap())
        .sum();
    assert_eq!(
        status["status"]["prune_review_count"].as_u64().unwrap(),
        report["total"].as_u64().unwrap() - similarity_total
    );
}

#[test]
fn all_similarity_categories_cap_after_dismissal_and_fingerprint_both_sources() {
    let f = fixture();
    let db = f.db();
    for id in 1..=8 {
        db.execute(
            "INSERT INTO facts(id,key,value,verified_at) VALUES (?1,?2,?3,CURRENT_TIMESTAMP)",
            params![id, format!("datastore-{id}"), CLAIM],
        )
        .unwrap();
        db.execute("INSERT INTO decisions(id,title,rationale) VALUES (?1,'Adopt Postgres as the datastore',?2)",
            params![id, CLAIM]).unwrap();
    }
    db.execute(
        "INSERT INTO facts(id,key,value,retired_at) VALUES (99,'old',?1,CURRENT_TIMESTAMP)",
        [CLAIM],
    )
    .unwrap();
    for id in 1..=19 {
        db.execute(
            "INSERT INTO tasks(id,title) VALUES (?1,?2)",
            params![id, CLAIM],
        )
        .unwrap();
    }
    drop(db);
    let before = f.scan();
    for (key, total, overflow) in [
        ("duplicate_facts", 28, 3),
        ("unlinked_decisions", 28, 3),
        ("correction_sweep", 27, 2),
    ] {
        assert_eq!(
            before["counts"][key],
            json!({"total": total, "overflow": overflow})
        );
        assert_eq!(category(&before, key).len(), 25);
    }
    let selected: Vec<_> = ["duplicate_facts", "unlinked_decisions", "correction_sweep"]
        .iter()
        .map(|name| category(&before, name)[0])
        .collect();
    for item in &selected {
        let keep = if item["category"] == "correction_sweep" {
            "keep"
        } else {
            "keep_both"
        };
        f.ok(&action(item, keep));
    }
    f.ok(&["prune", "dismiss", selected[0]["id"].as_str().unwrap()]);
    assert_eq!(
        f.db()
            .query_row("SELECT count(*) FROM prune_dismissals", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        f.db()
            .query_row(
                "SELECT count(*) FROM writes_log WHERE table_name='prune_dismissals'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    let after = f.scan();
    for item in &selected {
        assert!(!items(&after).iter().any(|i| i["id"] == item["id"]));
        let name = item["category"].as_str().unwrap();
        assert_eq!(category(&after, name).len(), 25);
        assert_eq!(
            after["counts"][name]["total"].as_u64().unwrap(),
            before["counts"][name]["total"].as_u64().unwrap() - 1
        );
    }
    f.db()
        .execute_batch(
            "UPDATE facts SET verified_at='2026-01-01' WHERE id=1;
         UPDATE decisions SET source='observed' WHERE id=1;
         UPDATE facts SET retired_reason='revised reason' WHERE id=99;",
        )
        .unwrap();
    let changed = f.scan();
    for item in selected {
        let name = item["category"].as_str().unwrap();
        assert_eq!(changed["counts"][name], before["counts"][name]);
        let new = category(&changed, name)
            .into_iter()
            .find(|i| i["subjects"] == item["subjects"])
            .unwrap();
        assert_ne!(new["id"], item["id"]);
        assert!(
            !f.run(&["prune", "dismiss", item["id"].as_str().unwrap()])
                .status
                .success()
        );
    }
}

#[test]
fn every_committed_supersession_and_retirement_reports_only_its_old_rows_sweep() {
    for case in [
        "fact",
        "decision",
        "accept_fact",
        "accept_decision",
        "staged_fact",
        "staged_decision",
        "retire",
    ] {
        let f = fixture();
        let db = f.db();
        for (id, key) in [(1, "datastore"), (2, "replacement")] {
            db.execute(
                "INSERT INTO facts(id,key,value,verified_at) VALUES (?1,?2,?3,CURRENT_TIMESTAMP)",
                params![id, key, CLAIM],
            )
            .unwrap();
            db.execute("INSERT INTO decisions(id,title,rationale) VALUES (?1,'Adopt Postgres as the datastore',?2)",
                params![id, CLAIM]).unwrap();
        }
        db.execute(
            "INSERT INTO tasks(id,title,notes) VALUES (1,?1,'Version 1.0 only')",
            [CLAIM],
        )
        .unwrap();
        db.execute("INSERT INTO facts(id,key,value,retired_at) VALUES (99,'other old',?1,CURRENT_TIMESTAMP)", [CLAIM]).unwrap();
        let decision = case.contains("decision");
        let kind = if decision { "decision" } else { "fact" };
        let payload = if case.starts_with("staged") {
            json!({"target_kind":kind,"old":"1","new":"2"})
        } else if decision {
            json!({"title":"Updated Postgres datastore"})
        } else {
            json!({"key":"new datastore","value":CLAIM})
        };
        db.execute(
            "INSERT INTO pending_writes(id,kind,payload_json,rationale,actor,actor_raw)
             VALUES (100,?1,?2,?3,'fixture','fixture')",
            params![
                if case.starts_with("staged") {
                    "supersede"
                } else {
                    kind
                },
                payload.to_string(),
                CLAIM
            ],
        )
        .unwrap();
        let before: i64 = db
            .query_row("SELECT count(*) FROM writes_log", [], |r| r.get(0))
            .unwrap();
        drop(db);
        let (report, replacement, expected_writes) = match case {
            "fact" | "decision" => {
                let out = f.json(&[kind, "supersede", "1", "--by", "2", "--json"]);
                assert_eq!(out["superseded"], true);
                (out["correction_sweep"].clone(), Some(2), 1)
            }
            "accept_fact" | "accept_decision" => {
                let out = f.json(&["review", "accept", "100", "--supersede", "1", "--json"]);
                (
                    out["correction_sweep"].clone(),
                    out["durable_id"].as_i64(),
                    3,
                )
            }
            "staged_fact" | "staged_decision" => {
                let out = f.json(&["review", "accept", "100", "--json"]);
                (out["correction_sweep"].clone(), Some(2), 2)
            }
            "retire" => {
                let human =
                    String::from_utf8(f.ok(&["fact", "retire", "1", "--reason", "obsolete"]))
                        .unwrap();
                for text in [
                    "Retired fact #1.",
                    "Correction sweep",
                    "old claim",
                    "possible restatement",
                    "Version 1.0 only",
                ] {
                    assert!(human.contains(text), "{human}");
                }
                (
                    serde_json::to_value(
                        memhub::commands::prune::correction_sweep(&f.repo, kind, 1).unwrap(),
                    )
                    .unwrap(),
                    None,
                    1,
                )
            }
            _ => unreachable!(),
        };
        let sweep = category(&report, "correction_sweep");
        assert!(!sweep.is_empty(), "{case}: {report}");
        assert!(
            sweep
                .iter()
                .any(|i| i["excerpts"]["possible_restatement"]["source_type"] == "task")
        );
        for item in sweep {
            assert_eq!(item["excerpts"]["old_claim"]["source_type"], kind);
            assert_eq!(item["excerpts"]["old_claim"]["source_id"], 1);
            let target = &item["excerpts"]["possible_restatement"];
            assert!(target["source_type"] != kind || target["source_id"].as_i64() != replacement);
        }
        let after: i64 = f
            .db()
            .query_row("SELECT count(*) FROM writes_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after - before,
            expected_writes,
            "{case}: reporting wrote extra rows"
        );
        let path = f.repo.join(".memhub/project.sqlite");
        let snapshot = fs::read(&path).unwrap();
        let repeated = memhub::commands::prune::correction_sweep(&f.repo, kind, 1).unwrap();
        assert_eq!(serde_json::to_value(repeated).unwrap(), report);
        assert_eq!(fs::read(path).unwrap(), snapshot);
    }
}

#[test]
fn disabled_post_change_report_is_structured_and_does_not_turn_success_into_failure() {
    let f = Fixture::new();
    f.db()
        .execute_batch(
            "INSERT INTO facts(id,key,value) VALUES (1,'old','value'),(2,'new','value');",
        )
        .unwrap();
    let result = f.json(&["fact", "supersede", "1", "--by", "2", "--json"]);
    assert_eq!(result["superseded"], true);
    assert_eq!(result["correction_sweep"]["total"], 0);
    assert!(
        result["correction_sweep"]["similarity_warning"]
            .as_str()
            .unwrap()
            .contains("skipped")
    );
}

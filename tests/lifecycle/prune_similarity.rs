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

fn assert_evidence_only(item: &Value) {
    assert_eq!(
        item["actions"],
        json!([{"action": "dismiss", "command": ["memhub", "prune", "dismiss", item["id"]]}]),
        "a similarity finding must offer only explicit dismissal: {}",
        item["actions"]
    );
}

#[test]
fn valid_similarity_dismissal_changes_only_dismissal_state() {
    assert_dismissal_skips_maintenance(true);
}

#[test]
fn rejected_similarity_dismissal_changes_nothing() {
    assert_dismissal_skips_maintenance(false);
}

fn assert_dismissal_skips_maintenance(valid: bool) {
    let f = fixture();
    f.db()
        .execute_batch(
            "INSERT INTO facts(key,value,verified_at) VALUES
                 ('database','We will run a Postgres server to hold project memory.',CURRENT_TIMESTAMP),
                 ('datastore','We will run a Postgres server to hold project memory.',CURRENT_TIMESTAMP);
             INSERT INTO pending_writes(kind,payload_json,rationale,status,actor,actor_raw,created_at)
                 VALUES ('fact','{}','aged fixture','pending','fixture','fixture','2000-01-01');
             UPDATE projects SET root_path = 'previous checkout';",
        )
        .unwrap();
    let before = memhub::commands::sync::LogicalVersion::read(&f.db()).unwrap();
    let db_path = f.repo.join(".memhub/project.sqlite");
    let before_bytes = fs::read(&db_path).unwrap();
    let report = f.scan();
    let pairs = category(&report, "duplicate_facts");
    assert_eq!(pairs.len(), 1);
    let id = pairs[0]["id"].as_str().unwrap();
    if valid {
        f.ok(&action(pairs[0], "dismiss"));
        assert!(category(&f.scan(), "duplicate_facts").is_empty());
    } else {
        let rejected = f.run(&["prune", "dismiss", "not-a-current-id"]);
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("no current prune candidate"));
    }

    let mut db = f.db();
    let tx = db.transaction().unwrap();
    let proposal: (String, Option<String>) = tx
        .query_row("SELECT status, reviewed_at FROM pending_writes", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(proposal, ("pending".to_string(), None));
    let root: String = tx
        .query_row("SELECT root_path FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(root, "previous checkout");
    assert_eq!(
        tx.execute(
            "DELETE FROM writes_log WHERE table_name = 'prune_dismissals' AND action = 'insert'
                 AND row_id = (SELECT id FROM prune_dismissals WHERE identifier = ?1)",
            [id],
        )
        .unwrap(),
        usize::from(valid)
    );
    assert_eq!(
        tx.execute("DELETE FROM prune_dismissals WHERE identifier = ?1", [id])
            .unwrap(),
        usize::from(valid)
    );
    assert_eq!(
        memhub::commands::sync::LogicalVersion::read(&tx).unwrap(),
        before
    );
    tx.rollback().unwrap();
    drop(db);
    if !valid {
        assert_eq!(fs::read(db_path).unwrap(), before_bytes);
    }
}

#[test]
fn impossible_decision_timestamp_is_evidence_only_in_json_and_text() {
    let f = fixture();
    let dates = ["2025-01-01T12:01:00Z", "2025-01-01T12:00:60Z"];
    for date in dates {
        f.db()
            .execute(
                "INSERT INTO decisions(title,rationale,decided_at)
                 VALUES ('Adopt Postgres as the datastore',?1,?2)",
                params![CLAIM, date],
            )
            .unwrap();
    }
    let report = f.scan();
    let pairs = category(&report, "unlinked_decisions");
    assert_eq!(pairs.len(), 1);
    let pair = pairs[0];
    assert_evidence_only(pair);
    // Display is in ID order, but the pinned PR's dismissal ID still resolves.
    assert_eq!(
        pair["id"],
        "a1762736ee752f1dbbc989a5979925c42caf8038823633905a382cbf8b8a6c54"
    );
    assert_eq!(pair["excerpts"]["first"]["recorded_at"], dates[0]);
    assert_eq!(pair["excerpts"]["second"]["recorded_at"], dates[1]);
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    for (side, id, date) in [("first", 1, dates[0]), ("second", 2, dates[1])] {
        assert!(human.contains(&format!("{side} match (decision #{id}, recorded {date})")));
    }
    assert!(!human.contains("decision supersede"), "{human}");
    assert!(!human.contains("chronology"), "{human}");
    f.ok(&action(pair, "dismiss"));
    assert!(category(&f.scan(), "unlinked_decisions").is_empty());
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
    // The paired evidence retains recorded dates even when they run against ID order.
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
    assert_evidence_only(duplicates[0]);
    let decisions = category(&report, "unlinked_decisions");
    assert_eq!(decisions.len(), 1);
    assert_evidence_only(decisions[0]);
    for (pair, kind) in [(duplicates[0], "fact"), (decisions[0], "decision")] {
        for (side, id) in [("first", 1), ("second", 2)] {
            let excerpt = &pair["excerpts"][side];
            assert_eq!(excerpt["source_type"], kind);
            assert_eq!(excerpt["source_id"], id);
            assert!(excerpt["recorded_at"].is_string());
            assert_eq!(excerpt["text"], CLAIM);
        }
    }
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
        assert_evidence_only(item);
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
            assert_eq!(old["text"], CLAIM);
            assert_eq!(old["summary"], "Version 1.0 only");
            assert_eq!(
                item["subjects"][0]["title"],
                "Adopt Postgres as the datastore"
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
        ("fact", CLAIM.to_owned()),
        ("doc_chunk", body.clone()),
        ("arch_section", body.clone()),
        ("task", CLAIM.to_owned()),
    ] {
        let hit = sweep
            .iter()
            .find(|i| i["excerpts"]["possible_restatement"]["source_type"] == kind)
            .unwrap();
        assert_eq!(
            hit["excerpts"]["possible_restatement"]["text"],
            expected_text
        );
        if kind == "task" {
            assert_eq!(
                hit["excerpts"]["possible_restatement"]["notes"],
                "Version 1.0 only"
            );
        }
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
    for command in [
        "memhub fact retire",
        "memhub decision supersede",
        "memhub doc add",
        "memhub arch set",
        "memhub task drop",
        "memhub task keep",
    ] {
        assert!(!human.contains(command), "scan advertised {command}");
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

fn imported_architecture(f: &Fixture) -> String {
    let body = format!("# Architecture v1.0 only, not v2.0\n\n## Storage\n\n{CLAIM}");
    let db = f.db();
    for (id, date, text) in [
        (10, "2025-01-01", "Cats are domesticated felines."),
        (11, "2025-01-01", body.as_str()),
        (12, "2020-01-01", "Dogs are domesticated canines."),
    ] {
        db.execute(
            "INSERT INTO project_arch(id,body,actor,actor_raw,created_at)
             VALUES (?1,?2,'fixture','fixture',?3)",
            params![id, text, date],
        )
        .unwrap();
    }
    drop(db);
    let export = f.repo.join("imported.json");
    f.ok(&["export", export.to_str().unwrap()]);
    f.ok(&["import", export.to_str().unwrap(), "--force"]);
    let shown = f.json(&["arch", "show", "--json"]);
    assert_eq!(shown["id"], 11);
    assert_eq!(shown["body"], body);
    body
}

#[test]
fn imported_decisions_keep_recorded_dates_without_ordering_the_evidence_by_time() {
    let f = fixture();
    let dates = [
        "2025-01-01t00:00:00.0000000002z",
        "2025-01-01 00:00:00.0000000001",
        "2025-01-03",
        "2025-01-02T01:00:00.0001+02:00",
        "2025-01-03T00:00:00.00010Z",
        "2025-01-03 01:00:00.000100+01:00",
        "unknown",
        "2025-01-01T",
    ];
    for (index, date) in dates.iter().enumerate() {
        f.db()
            .execute(
                "INSERT INTO decisions(id,title,rationale,decided_at)
                 VALUES (?1,'Adopt Postgres as the datastore',?2,?3)",
                params![index + 1, CLAIM, date],
            )
            .unwrap();
    }
    let export = f.repo.join("decisions.json");
    f.ok(&["export", export.to_str().unwrap()]);
    f.ok(&["import", export.to_str().unwrap(), "--force"]);
    let report = f.scan();
    let decisions = category(&report, "unlinked_decisions");
    assert_eq!(report["counts"]["unlinked_decisions"]["total"], 28);
    for pair in decisions {
        let first = &pair["excerpts"]["first"];
        let second = &pair["excerpts"]["second"];
        let a = first["source_id"].as_u64().unwrap() as usize;
        let b = second["source_id"].as_u64().unwrap() as usize;
        assert!(a < b, "{pair}");
        assert_eq!(first["recorded_at"], dates[a - 1]);
        assert_eq!(second["recorded_at"], dates[b - 1]);
        assert_eq!(
            pair["reason"],
            "Textual overlap meets the accept-time similarity cutoff; not evidence of contradiction."
        );
        assert_evidence_only(pair);
    }
    assert_eq!(f.scan(), report);
}

#[test]
fn unusable_imported_targets_stay_reviewable_without_corrective_commands() {
    let f = fixture();
    f.db()
        .execute_batch(&format!(
            "INSERT INTO facts(id,key,value,verified_at,retired_at) VALUES
               (-1,'database','{CLAIM}',CURRENT_TIMESTAMP,NULL),
               (1,'datastore','{CLAIM}',CURRENT_TIMESTAMP,NULL),
               (2,'old','{CLAIM}',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP);
             INSERT INTO tasks(id,title) VALUES (-1,'{CLAIM}');
             INSERT INTO decisions(id,title,rationale,decided_at) VALUES
               (1,'Adopt Postgres as the datastore','{CLAIM}','2025-01-01'),
               (-1,'Adopt Postgres as the datastore','{CLAIM}','2025-01-02');
             INSERT INTO project_arch(body,actor,actor_raw,created_at) VALUES
               ('{CLAIM}','fixture','fixture','9999-01-01T00:00:00Z');"
        ))
        .unwrap();
    let export = f.repo.join("legacy.json");
    f.ok(&["export", export.to_str().unwrap()]);
    f.ok(&["import", export.to_str().unwrap(), "--force"]);
    let report = f.scan();
    let duplicate = category(&report, "duplicate_facts")[0];
    assert_evidence_only(duplicate);
    assert_eq!(duplicate["excerpts"]["first"]["source_id"], -1);
    let decision = category(&report, "unlinked_decisions")[0];
    assert_evidence_only(decision);
    assert_eq!(decision["excerpts"]["first"]["source_id"], -1);
    for (kind, id) in [("fact", -1), ("task", -1), ("arch_section", 1)] {
        let hit = category(&report, "correction_sweep")
            .into_iter()
            .find(|item| {
                let target = &item["excerpts"]["possible_restatement"];
                target["source_type"] == kind && target["source_id"] == id
            })
            .unwrap_or_else(|| panic!("missing {kind} #{id} in {report}"));
        assert_evidence_only(hit);
        assert!(
            hit["excerpts"]["possible_restatement"]["text"]
                .as_str()
                .unwrap()
                .contains(CLAIM)
        );
        f.ok(&action(hit, "dismiss"));
    }
}

#[test]
fn legacy_and_current_architecture_excerpts_never_borrow_an_adjacent_versions_context() {
    for legacy in [true, false] {
        let f = fixture();
        let wrapper = if legacy {
            "# Architecture for C#"
        } else {
            "Architecture for C# only\n========================="
        };
        let body = format!(
            "{wrapper}\n\n## Storage v1.0 only\n\n{CLAIM}\n\n## Storage v2.0 only\n\n{CLAIM}"
        );
        if legacy {
            // A supported pre-existing layout retains the wrapper at ord 0.
            // Insert it directly: no export/import or section rederivation.
            let db = f.db();
            db.execute(
                "INSERT INTO project_arch(id,body,actor,actor_raw)
                 VALUES (1,?1,'fixture','fixture')",
                [&body],
            )
            .unwrap();
            for (ord, (heading, chunk)) in memhub::commands::doc::chunk_markdown(&body)
                .iter()
                .enumerate()
            {
                db.execute(
                    "INSERT INTO arch_sections(arch_id,ord,heading_path,body) VALUES (1,?1,?2,?3)",
                    params![ord as i64, heading, chunk],
                )
                .unwrap();
            }
        } else {
            f.ok(&["arch", "set", "--", &body]);
        }
        f.db()
            .execute(
                "INSERT INTO facts(id,key,value) VALUES (1,'old',?1)",
                [CLAIM],
            )
            .unwrap();
        let post_human =
            String::from_utf8(f.ok(&["fact", "retire", "1", "--reason", "obsolete"])).unwrap();
        let post = serde_json::to_value(
            memhub::commands::prune::correction_sweep(&f.repo, "fact", 1).unwrap(),
        )
        .unwrap();
        let scan = f.scan();
        let hits = category(&scan, "correction_sweep");
        assert_eq!(category(&post, "correction_sweep"), hits);
        assert_eq!(hits.len(), 2, "{scan}");
        for (index, hit) in hits.iter().enumerate() {
            let version = index + 1;
            let excerpt = &hit["excerpts"]["possible_restatement"];
            assert_eq!(
                excerpt["text"],
                format!("## Storage v{version}.0 only\n\n{CLAIM}")
            );
            assert_eq!(excerpt["architecture_source"]["text"], body);
            assert_eq!(excerpt["architecture_source"]["architecture_id"], 1);
            assert_eq!(excerpt["architecture_source"]["section_ordinal"], version);
        }
        let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
        for output in [&human, &post_human] {
            assert!(
                output.contains(&wrapper.replace('\n', "\n      ")),
                "{output}"
            );
            assert!(output.contains("Architecture #1 full source (matched stored section ordinal"));
        }
        for hit in hits {
            let excerpt = hit["excerpts"]["possible_restatement"]["text"]
                .as_str()
                .unwrap()
                .replace('\n', "\n      ");
            assert!(human.contains(&excerpt), "{human}");
            assert!(post_human.contains(&excerpt), "{post_human}");
        }
        assert_eq!(
            f.db()
                .query_row("SELECT count(*) FROM arch_sections", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3,
            "reporting must not normalize the stored layout"
        );
    }
}

#[test]
fn repeated_architecture_headings_use_their_exact_body_and_preserve_wrapper_qualifiers() {
    let f = fixture();
    let body = format!(
        "# Architecture v1.0 only\n\n## Storage\n\n{CLAIM}\n\n## Storage\n\n{CLAIM}\n\n## Storage\n\n{CLAIM}\n\nFor v2.0 only."
    );
    f.ok(&["arch", "set", "--", &body]);
    f.db()
        .execute(
            "INSERT INTO facts(id,key,value) VALUES (1,'old',?1)",
            [CLAIM],
        )
        .unwrap();
    let post_human =
        String::from_utf8(f.ok(&["fact", "retire", "1", "--reason", "obsolete"])).unwrap();
    let post = serde_json::to_value(
        memhub::commands::prune::correction_sweep(&f.repo, "fact", 1).unwrap(),
    )
    .unwrap();
    let scan = f.scan();
    let hits = category(&scan, "correction_sweep");
    assert_eq!(category(&post, "correction_sweep"), hits);
    assert_eq!(hits.len(), 3);
    for hit in &hits[..2] {
        assert_eq!(
            hit["excerpts"]["possible_restatement"]["text"],
            format!("## Storage\n\n{CLAIM}")
        );
    }
    assert_eq!(
        hits[2]["excerpts"]["possible_restatement"]["text"],
        format!("## Storage\n\n{CLAIM}\n\nFor v2.0 only.")
    );
    for (index, hit) in hits.into_iter().enumerate() {
        let restatement = &hit["excerpts"]["possible_restatement"];
        assert_eq!(restatement["architecture_source"]["text"], body);
        assert_eq!(restatement["architecture_source"]["section_ordinal"], index);
        let excerpt = restatement["text"]
            .as_str()
            .unwrap()
            .replace('\n', "\n      ");
        assert!(post_human.contains(&excerpt), "{post_human}");
    }
}

#[test]
fn correction_uses_current_imported_architecture_and_preserves_enclosing_version_heading() {
    let f = fixture();
    let body = imported_architecture(&f);
    f.db()
        .execute(
            "INSERT INTO facts(id,key,value,retired_at) VALUES (1,'old',?1,CURRENT_TIMESTAMP)",
            [CLAIM],
        )
        .unwrap();
    let report = f.scan();
    let hits = category(&report, "correction_sweep");
    let hit = hits
        .iter()
        .find(|i| i["excerpts"]["possible_restatement"]["source_type"] == "arch_section")
        .unwrap();
    assert_eq!(
        hit["excerpts"]["possible_restatement"]["text"],
        format!("## Storage\n\n{CLAIM}")
    );
    let source = &hit["excerpts"]["possible_restatement"]["architecture_source"];
    assert_eq!(source["text"], body);
    assert_eq!(source["architecture_id"], 11);
    assert_eq!(source["section_ordinal"], 0);
    assert_eq!(
        hit["excerpts"]["possible_restatement"]["recorded_at"],
        "2025-01-01"
    );
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert!(
        human.contains("Architecture v1.0 only, not v2.0"),
        "{human}"
    );
    assert_evidence_only(hit);
    assert!(!human.contains("memhub arch set"), "{human}");
}

#[test]
fn stored_document_matches_survive_redirected_paths_and_invalidate_dismissal() {
    let f = fixture();
    let source = f.repo.join("source");
    let moved = f.repo.join("moved");
    let target = f.repo.join("target");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    let doc = source.join("guide.md");
    fs::write(
        &doc,
        format!("# Guide v1.0 only ###\n\n## Storage for C#\n\n{CLAIM}"),
    )
    .unwrap();
    f.ok(&["doc", "add", doc.to_str().unwrap()]);
    f.db()
        .execute(
            "INSERT INTO facts(id,key,value,retired_at) VALUES (1,'old',?1,CURRENT_TIMESTAMP)",
            [CLAIM],
        )
        .unwrap();
    let before = f.scan();
    let before_hit = category(&before, "correction_sweep")[0];
    assert_evidence_only(before_hit);
    f.ok(&action(before_hit, "dismiss"));
    fs::rename(&source, &moved).unwrap();
    fs::write(target.join("guide.md"), "Unrelated destination.").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &source).unwrap();
    #[cfg(windows)]
    {
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&source)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let db_path = f.repo.join(".memhub/project.sqlite");
    let snapshot = fs::read(&db_path).unwrap();
    let report = f.scan();
    let hits = category(&report, "correction_sweep");
    let hit = hits[0];
    assert_ne!(hit["id"], before_hit["id"]);
    assert_eq!(
        hit["subjects"][1]["heading_context"],
        "Guide v1.0 only > Storage for C"
    );
    assert_eq!(
        hit["excerpts"]["possible_restatement"]["text"],
        format!("## Storage for C#\n\n{CLAIM}"),
    );
    assert!(
        hit["excerpts"]["possible_restatement"]["original_wording_unavailable"]
            .as_str()
            .unwrap()
            .contains("exact ancestor headings")
    );
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    assert!(human.contains("heading context (derived)"));
    assert!(human.contains("Original wording unavailable"));
    assert!(human.contains("## Storage for C#"));
    assert!(!human.contains("# Guide v1.0 only ###"));
    assert_evidence_only(hit);
    assert_eq!(fs::read(&db_path).unwrap(), snapshot);
    let post = serde_json::to_value(
        memhub::commands::prune::correction_sweep(&f.repo, "fact", 1).unwrap(),
    )
    .unwrap();
    assert_eq!(category(&post, "correction_sweep"), hits);
    f.ok(&action(hit, "dismiss"));
    assert!(category(&f.scan(), "correction_sweep").is_empty());
    assert_eq!(
        fs::read_to_string(target.join("guide.md")).unwrap(),
        "Unrelated destination."
    );
}

#[test]
fn oversized_document_title_is_evidence_only_in_scan_and_post_change() {
    let f = fixture();
    let doc = f.repo.join("guide.md");
    // Exceeds both Windows' command-line budget and Linux's 128 KiB
    // per-argument limit on systems with 4 KiB pages.
    let title = "Long title ".repeat(12_000);
    let excerpt = format!("# Storage\n\n{CLAIM}");
    fs::write(&doc, format!("# {title}\n\n{excerpt}")).unwrap();
    let ingested = f.json(&["doc", "add", "--json", "guide.md"]);
    assert_eq!(ingested["title"], title.trim_end());
    f.db()
        .execute("INSERT INTO facts(key,value) VALUES ('old',?1)", [CLAIM])
        .unwrap();
    let post_human =
        String::from_utf8(f.ok(&["fact", "retire", "1", "--reason", "obsolete"])).unwrap();
    let report = f.scan();
    let hit = category(&report, "correction_sweep")
        .into_iter()
        .find(|item| item["excerpts"]["possible_restatement"]["text"] == excerpt)
        .expect("the stored matching chunk must still be reported");
    assert_eq!(hit["subjects"][1]["document_id"], ingested["id"]);
    assert_eq!(hit["subjects"][1]["title"], title.trim_end());
    assert_eq!(
        hit["reason"],
        "Textual overlap with an old claim; not evidence of contradiction."
    );
    assert_evidence_only(hit);
    let post = serde_json::to_value(
        memhub::commands::prune::correction_sweep(&f.repo, "fact", 1).unwrap(),
    )
    .unwrap();
    assert_eq!(category(&post, "correction_sweep"), vec![hit]);
    let human = String::from_utf8(f.ok(&["prune", "scan"])).unwrap();
    for output in [&human, &post_human] {
        assert!(output.contains("possible restatement (doc_chunk"));
        assert!(output.contains(&format!(
            "dismiss: memhub prune dismiss {}",
            hit["id"].as_str().unwrap()
        )));
        assert!(
            !output.contains("memhub doc add"),
            "report offered re-ingest"
        );
    }
    f.ok(&action(hit, "dismiss"));
    assert!(category(&f.scan(), "correction_sweep").is_empty());

    #[cfg(windows)]
    {
        fs::write(&doc, "Edited on disk.").unwrap();
        let report = f.scan();
        let out_of_date = row_item(
            &report,
            "out_of_date_document",
            ingested["id"].as_i64().unwrap(),
        );
        assert_eq!(
            out_of_date["actions"].as_array().unwrap().len(),
            1,
            "the original drift category must still guard the Windows command-line budget"
        );
        assert_eq!(&action(out_of_date, "keep")[..2], ["prune", "dismiss"]);
        assert!(
            out_of_date["reason"]
                .as_str()
                .unwrap()
                .contains("command line")
        );
        f.ok(&action(out_of_date, "keep"));
        assert!(category(&f.scan(), "out_of_date_document").is_empty());
    }
}

#[test]
fn document_actions_from_a_subdirectory_only_target_the_canonical_ingest_row() {
    let f = fixture();
    let nested = f.repo.join("nested");
    fs::create_dir(&nested).unwrap();
    let doc = f.repo.join("guide.md");
    let body = format!("# Guide v1.0 only\n\n{CLAIM}");
    fs::write(&doc, &body).unwrap();
    fs::write(nested.join("guide.md"), "A different document.").unwrap();
    fs::write(f.repo.join("legacy.md"), &body).unwrap();
    let original = f.json(&["doc", "add", "--json", "guide.md"]);
    let db = f.db();
    db.execute_batch(
        "INSERT INTO documents(id,path,title,content_hash,byte_len) VALUES
             (20,'guide.md','Relative alias','old',1),
             (21,'legacy.md','Relative only','old',1);",
    )
    .unwrap();
    for row in [20, 21] {
        db.execute(
            "INSERT INTO doc_chunks(doc_id,ord,heading_path,body) VALUES (?1,0,'Guide v1.0 only',?2)",
            params![row, body],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO facts(key,value,retired_at) VALUES ('old',?1,CURRENT_TIMESTAMP)",
        [CLAIM],
    )
    .unwrap();
    drop(db);
    fs::write(&doc, format!("{body}\n\nEdited.")).unwrap();

    let run = |args: &[&str]| {
        let out = f
            .command(env!("CARGO_BIN_EXE_memhub"))
            .current_dir(&nested)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let report = run(&["prune", "scan", "--json"])["prune_scan"].clone();
    assert_eq!(report, f.scan());
    for id in [20, 21] {
        let item = category(&report, "correction_sweep")
            .into_iter()
            .find(|item| item["subjects"][1]["document_id"] == id)
            .unwrap();
        assert!(
            item["excerpts"]["possible_restatement"]["text"]
                .as_str()
                .unwrap()
                .contains(&body)
        );
        assert_evidence_only(item);
        let drift = row_item(&report, "out_of_date_document", id);
        assert_eq!(drift["actions"].as_array().unwrap().len(), 1);
        assert_eq!(&action(drift, "keep")[..2], ["prune", "dismiss"]);
        assert!(
            drift["reason"]
                .as_str()
                .unwrap()
                .contains("stored path is relative")
        );
    }
    let item = category(&report, "correction_sweep")
        .into_iter()
        .find(|item| item["subjects"][1]["document_id"] == original["id"])
        .unwrap();
    assert_evidence_only(item);
    let drift = row_item(
        &report,
        "out_of_date_document",
        original["id"].as_i64().unwrap(),
    );
    let mut argv = action(drift, "re_ingest");
    assert_eq!(
        argv.last().copied(),
        fs::canonicalize(&doc).unwrap().to_str()
    );
    argv.insert(2, "--json");
    let updated = run(&argv);
    assert_eq!(updated["status"], "updated");
    assert_eq!(updated["id"], original["id"]);
    assert_eq!(
        f.db()
            .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3,
        "neither a legacy alias nor a subdirectory file should be ingested"
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
        assert_evidence_only(item);
        f.ok(&action(item, "dismiss"));
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
        imported_architecture(&f);
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
                    "Architecture v1.0 only, not v2.0",
                ] {
                    assert!(human.contains(text), "{human}");
                }
                for command in ["memhub task drop", "memhub task keep", "memhub arch set"] {
                    assert!(!human.contains(command), "{case}: {human}");
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
        let architecture = sweep
            .iter()
            .find(|i| i["excerpts"]["possible_restatement"]["source_type"] == "arch_section")
            .unwrap();
        assert!(
            architecture["excerpts"]["possible_restatement"]["architecture_source"]["text"]
                .as_str()
                .unwrap()
                .contains("Architecture v1.0 only, not v2.0")
        );
        assert_eq!(
            architecture["excerpts"]["possible_restatement"]["recorded_at"],
            "2025-01-01"
        );
        for item in sweep {
            assert_evidence_only(item);
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

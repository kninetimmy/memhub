//! `memhub eval self-check` (issue #383): reports per row type and query
//! form how many sampled rows come back first / in the top 3, lists the
//! misses, draws a stable sample, and is read-only.
//!
//! Seeds a disposable hybrid-mode project (hybrid *before* seeding, so
//! eager-embed fires) and drives the compiled binary with `--json`, the
//! same pattern as `retrieval_golden_hermetic`. One fixture and one
//! test keep the number of model loads small.

use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::commands::{decision, doc, fact, init, narrative, session_note, task};
use memhub::config::{ProjectConfig, RetrievalMode};
use memhub::models::NarrativeKind;
use serde_json::Value;
use tempfile::tempdir;

const TABLES: [&str; 10] = [
    "facts",
    "decisions",
    "tasks",
    "session_notes",
    "documents",
    "doc_chunks",
    "project_arch",
    "arch_sections",
    "writes_log",
    "recall_metrics",
];

fn seed(root: &Path) {
    init::run(root).expect("init");
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    // Everything that could log a recall is on, so a leak would show.
    config.retrieval.log_queries = true;
    config.metrics.enabled = true;
    config.save(&config_path).expect("save config");

    for (key, value) in [
        (
            "build.toolchain",
            "The release binary is built with the stable Rust toolchain pinned in rust-toolchain.toml",
        ),
        (
            "ci.cache.strategy",
            "Continuous integration caches the cargo registry and the downloaded model assets between runs",
        ),
    ] {
        fact::add(root, key, value, "user", "cli:user").expect("fact");
    }
    decision::add(
        root,
        "Use optimistic locking for the ledger export",
        "Concurrent exports compare a version counter before writing so that a slow exporter \
         never overwrites a newer ledger snapshot produced by another process.",
        "user",
        "cli:user",
    )
    .expect("decision 1");
    decision::add(
        root,
        "Retire the legacy badge renderer",
        "The badge renderer duplicated the status table and drifted from it, so the table is \
         now the only supported presentation of project health indicators.",
        "user",
        "cli:user",
    )
    .expect("decision 2");
    task::add(
        root,
        "Migrate the telemetry shim to the new exporter",
        Some(
            "Replace every direct call to the shim with the exporter interface and delete \
             the shim afterwards.",
        ),
        "cli:user",
    )
    .expect("task 1");
    // No notes: has no passage query, so it must not be counted for one.
    task::add(root, "Rotate the staging signing keys", None, "cli:user").expect("task 2");
    session_note::add(
        root,
        "scratch note that recall never searches by default",
        "cli:user",
        "cli:user",
    )
    .expect("note");

    let doc_file = root.join("ops-guide.md");
    fs::write(
        &doc_file,
        "# Operations Guide\n\n\
         ## Backup Rotation\n\n\
         Nightly snapshots are kept for fourteen days and weekly snapshots for six months, \
         after which the oldest archive is pruned by the retention job.\n\n\
         ## Pager Escalation\n\n\
         An unacknowledged page escalates to the secondary on-call after fifteen minutes and \
         to the engineering manager after thirty.\n",
    )
    .expect("write doc");
    doc::add(root, &doc_file, None, "cli:user").expect("doc add");

    narrative::set(
        root,
        NarrativeKind::Arch,
        "# Architecture\n\n\
         ## Ingestion Pipeline\n\n\
         Raw events enter through the collector, are validated against the schema registry and \
         then fan out to the enrichment workers over a bounded queue.\n\n\
         ## Storage Layout\n\n\
         Enriched events land in time-partitioned tables with a secondary index per tenant so \
         that retention can drop whole partitions cheaply.\n",
        "cli:user",
        "cli:user",
    )
    .expect("arch set");
}

fn memhub(root: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .output()
        .expect("spawn memhub")
}

fn counts(root: &Path) -> Vec<i64> {
    let conn = rusqlite::Connection::open(root.join(".memhub").join("project.sqlite")).unwrap();
    TABLES
        .iter()
        .map(|t| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        })
        .collect()
}

#[test]
fn self_check_reports_per_type_and_form_is_stable_and_read_only() {
    let temp = tempdir().expect("tempdir");
    let home = tempdir().expect("home tempdir");
    let (root, home) = (temp.path(), home.path());
    seed(root);

    // Negative control: a plain agent-facing recall does append to the
    // query log, so an unchanged log below means self-check really skipped it.
    let control = memhub(
        root,
        home,
        &["recall", "ledger export locking", "--mode", "fts"],
    );
    assert!(control.status.success());
    let log_path = root.join(".memhub").join("recall_queries.jsonl");
    let log_before =
        fs::read_to_string(&log_path).expect("recall must have logged the control query");
    let counts_before = counts(root);

    let first = memhub(
        root,
        home,
        &["eval", "self-check", "--sample", "2", "--json"],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = memhub(
        root,
        home,
        &["eval", "self-check", "--sample", "2", "--json"],
    );
    assert!(second.status.success());

    // Same database, same sample, same report.
    assert_eq!(first.stdout, second.stdout, "repeated runs must agree");

    // Read-only: no row added or removed anywhere, no query-log line.
    assert_eq!(
        counts(root),
        counts_before,
        "self-check must not write rows"
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap(), log_before);

    let report: Value = serde_json::from_slice(&first.stdout).expect("json");
    assert_eq!(report["sample"], 2);
    assert_eq!(report["top_k"], 3);
    let types = report["types"].as_array().unwrap();
    let names: Vec<_> = types
        .iter()
        .map(|t| t["source_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["fact", "decision", "task", "doc_chunk", "arch_section"]
    );

    let mut miss_count = 0;
    for t in types {
        let name = t["source_type"].as_str().unwrap();
        assert_eq!(t["sampled"], 2, "{name}");
        for form in ["title", "passage"] {
            let c = &t[form];
            let (queried, first, top3) = (
                c["queried"].as_u64().unwrap(),
                c["first"].as_u64().unwrap(),
                c["top3"].as_u64().unwrap(),
            );
            assert!(first <= top3 && top3 <= queried, "{name} {form}: {c}");
            miss_count += queried - top3;
        }
    }
    // The two decisions have distinctive titles and text: both forms find both.
    let decision = &types[1];
    assert_eq!(decision["title"]["top3"], 2, "{decision}");
    assert_eq!(decision["passage"]["top3"], 2, "{decision}");
    // A task with no notes has no passage query, and so is not counted.
    assert_eq!(types[2]["title"]["queried"], 2);
    assert_eq!(types[2]["passage"]["queried"], 1);

    // The misses list is exactly the rows outside the top 3.
    let misses = report["misses"].as_array().unwrap();
    assert_eq!(misses.len() as u64, miss_count);
    for m in misses {
        assert!(
            m["rank"].is_null() || m["rank"].as_u64().unwrap() > 3,
            "{m}"
        );
        assert!(["title", "passage"].contains(&m["form"].as_str().unwrap()));
    }

    // Human output names every type.
    let human = memhub(root, home, &["eval", "self-check", "--sample", "1"]);
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    for name in ["fact", "decision", "task", "doc_chunk", "arch_section"] {
        assert!(text.contains(name), "{text}");
    }
}

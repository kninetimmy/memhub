//! Isolation + end-to-end coverage for the session-transcript archiver
//! (Wave 6 W3, issue #96). The tier:opus core here is the *exclusion*
//! invariants — a transcript is a compressed file plus a pointer row and
//! must never leak into embedding, recall, or export/import. These tests
//! are modeled on the docs / M11 isolation tests (`export_excludes_*`,
//! the code-index "never read by recall" posture).

use std::fs;
use std::path::Path;

use memhub::commands::{export, fact, import, init, search, transcript};
use memhub::config::ProjectConfig;
use memhub::db;
use tempfile::tempdir;

/// A distinctive token planted in the transcript body so a leak into any
/// searchable/exported surface is unambiguous.
const MARKER: &str = "TRANSCRIPTSECRETMARKER42";

fn set_claude_dir(repo: &Path, dir: &Path) {
    let config_path = repo.join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load config");
    cfg.metrics.claude_transcripts_dir = dir.to_string_lossy().into_owned();
    cfg.save(&config_path).expect("save config");
}

/// Init a repo, point the Claude transcripts dir at a temp dir, drop a
/// session JSONL carrying the unique marker, and archive it (approved).
fn archive_a_marked_transcript(repo: &Path) -> transcript::ArchiveReport {
    init::run(repo).expect("init");
    let tdir = repo.join("claude-transcripts");
    fs::create_dir_all(&tdir).expect("tdir");
    let source = tdir.join("sess-iso.jsonl");
    fs::write(
        &source,
        format!("{{\"type\":\"assistant\",\"secret\":\"{MARKER}\"}}\n"),
    )
    .expect("write transcript");
    set_claude_dir(repo, &tdir);
    transcript::archive(repo, transcript::Agent::Claude, "sess-iso", true).expect("archive")
}

#[test]
fn archive_writes_a_compressed_copy_under_memhub_and_a_pointer_row() {
    let temp = tempdir().expect("tempdir");
    let report = archive_a_marked_transcript(temp.path());

    assert!(report.archive_path.exists());
    assert!(
        report
            .archive_path
            .to_string_lossy()
            .ends_with(".jsonl.zst")
    );
    assert!(
        report
            .archive_path
            .starts_with(temp.path().join(".memhub").join("transcripts")),
        "archive must land under .memhub/transcripts/: {}",
        report.archive_path.display()
    );

    let ctx = db::open_project(temp.path()).expect("open");
    let count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM session_transcripts WHERE session_id = 'sess-iso'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "exactly one pointer row for the archived session");
}

#[test]
fn archived_transcript_is_never_embedded() {
    let temp = tempdir().expect("tempdir");
    archive_a_marked_transcript(temp.path());

    let ctx = db::open_project(temp.path()).expect("open");
    // The invariant: no embedding row is ever attributed to a transcript.
    // There is no `SourceType::Transcript`, and the archive path never
    // calls the eager-embed writers, so this count is structurally zero.
    let transcript_embeddings: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM embeddings WHERE source_type = 'transcript'",
            [],
            |r| r.get(0),
        )
        .expect("count transcript embeddings");
    assert_eq!(
        transcript_embeddings, 0,
        "transcripts must never enter the embeddings table"
    );
}

#[test]
fn archived_transcript_is_never_in_recall_or_search() {
    let temp = tempdir().expect("tempdir");
    archive_a_marked_transcript(temp.path());

    // FTS search (hermetic; no embedding model) for the unique marker
    // returns nothing: a transcript lives in no FTS-backed source table,
    // so recall/search cannot surface it.
    let response = search::run(temp.path(), MARKER, 10).expect("search runs");
    assert!(
        response.results.is_empty(),
        "transcript content leaked into search: {:?}",
        response.results
    );
}

#[test]
fn archived_transcript_is_excluded_from_export() {
    let temp = tempdir().expect("tempdir");
    archive_a_marked_transcript(temp.path());

    let dest = temp.path().join("export.json");
    export::run(temp.path(), &dest).expect("export succeeds");
    let raw = fs::read_to_string(&dest).expect("read export");

    // No session_transcripts key in the export shape ...
    let object = serde_json::from_str::<serde_json::Value>(&raw)
        .expect("parse json")
        .as_object()
        .expect("top-level object")
        .clone();
    assert!(
        !object.contains_key("session_transcripts"),
        "export must not carry a session_transcripts table"
    );
    // ... and the raw transcript content never appears anywhere in it.
    assert!(
        !raw.contains(MARKER),
        "transcript content leaked into the export payload"
    );
}

#[test]
fn transcript_archive_does_not_travel_through_import() {
    let source = tempdir().expect("source tempdir");
    archive_a_marked_transcript(source.path());
    // A durable row so the export/import path has something to carry.
    fact::add(source.path(), "build", "cargo build", "user", "cli:user").expect("fact");

    let export_path = source.path().join("export.json");
    export::run(source.path(), &export_path).expect("export");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");
    import::run(target.path(), &export_path, false).expect("import");

    let ctx = db::open_project(target.path()).expect("open target");
    let count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM session_transcripts", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0, "import must not materialize any transcript rows");
    // The durable data still crossed — proving the import actually ran.
    let facts = fact::list(target.path()).expect("list facts");
    assert_eq!(facts.len(), 1);
}

#[test]
fn archive_errors_clearly_for_a_missing_session() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let tdir = temp.path().join("claude-transcripts");
    fs::create_dir_all(&tdir).expect("tdir");
    set_claude_dir(temp.path(), &tdir);

    let err = transcript::archive(temp.path(), transcript::Agent::Claude, "nope", true)
        .expect_err("must error on a missing transcript");
    assert!(
        err.to_string().contains("no Claude transcript found"),
        "unexpected error: {err}"
    );
}

#[test]
fn archive_refuses_without_explicit_approval_even_with_a_valid_session() {
    let temp = tempdir().expect("tempdir");
    let tdir = temp.path().join("claude-transcripts");
    init::run(temp.path()).expect("init");
    fs::create_dir_all(&tdir).expect("tdir");
    fs::write(tdir.join("sess-x.jsonl"), "{}\n").expect("write");
    set_claude_dir(temp.path(), &tdir);

    let err = transcript::archive(temp.path(), transcript::Agent::Claude, "sess-x", false)
        .expect_err("must refuse without approval");
    assert!(err.to_string().contains("explicit approval"), "{err}");

    // Nothing was written: the refusal is total, not partial.
    let ctx = db::open_project(temp.path()).expect("open");
    let count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM session_transcripts", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 0);
    assert!(!temp.path().join(".memhub").join("transcripts").exists());
}

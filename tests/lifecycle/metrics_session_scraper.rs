//! Integration coverage for Component B of the token-accounting
//! subsystem (decision 74, task #29): the Claude Code transcript
//! scraper wired into `db::open_project`.
//!
//! Every test bootstraps a real temp project, opts metrics in via the
//! on-disk config (off by default everywhere else), drops a transcript
//! under a temp transcripts dir, and asserts what landed in
//! `session_metrics` / `writes_log` after `open_project` ran the
//! opportunistic scrape.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use memhub::commands::init;
use memhub::config::ProjectConfig;
use memhub::db;
use tempfile::tempdir;

// Wave 5 U4 (issue #90): this file moved from `tests/` to `tests/lifecycle/`
// when the standalone `tests/*.rs` binaries were folded into shared harness
// binaries, so the fixture (still at `tests/fixtures/`, untouched) is now one
// level further up.
const FIXTURE: &str = include_str!("../fixtures/claude_session_sample.jsonl");

struct Row {
    agent: String,
    started_at: String,
    ended_at: Option<String>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    recall_calls: i64,
    offset: i64,
}

fn enable_metrics(repo: &Path, transcripts_dir: &Path) {
    let config_path = repo.join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load config");
    cfg.metrics.enabled = true;
    cfg.metrics.session_accounting = true;
    cfg.metrics.claude_transcripts_dir = transcripts_dir.to_string_lossy().into_owned();
    cfg.save(&config_path).expect("save config");
}

/// Open the project (which runs the opportunistic scrape) and read the
/// single session row back. Returns `None` if no row exists yet.
fn scrape_and_read(repo: &Path, session_id: &str) -> Option<Row> {
    let ctx = db::open_project(repo).expect("open_project");
    ctx.conn
        .query_row(
            "SELECT agent, started_at, ended_at, input_tokens, output_tokens, \
                    cache_read_tokens, cache_creation_tokens, recall_calls, \
                    last_scanned_offset \
             FROM session_metrics WHERE session_id = ?1",
            [session_id],
            |r| {
                Ok(Row {
                    agent: r.get(0)?,
                    started_at: r.get(1)?,
                    ended_at: r.get(2)?,
                    input: r.get(3)?,
                    output: r.get(4)?,
                    cache_read: r.get(5)?,
                    cache_creation: r.get(6)?,
                    recall_calls: r.get(7)?,
                    offset: r.get(8)?,
                })
            },
        )
        .ok()
}

fn writes_log_skip_count(repo: &Path) -> i64 {
    let ctx = db::open_project(repo).expect("open_project");
    ctx.conn
        .query_row(
            "SELECT COUNT(*) FROM writes_log \
             WHERE actor = 'metrics:claude-scraper' AND action = 'scrape_skip'",
            [],
            |r| r.get(0),
        )
        .expect("count writes_log")
}

fn append(path: &Path, bytes: &str) {
    let mut f = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append open");
    f.write_all(bytes.as_bytes()).expect("append write");
}

#[test]
fn scrapes_claude_fixture_into_session_metrics() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let tdir = temp.path().join("transcripts");
    fs::create_dir_all(&tdir).expect("transcripts dir");
    let file = tdir.join("sess-abc.jsonl");
    fs::write(&file, FIXTURE).expect("write fixture");
    enable_metrics(temp.path(), &tdir);

    let row = scrape_and_read(temp.path(), "sess-abc").expect("row exists");

    // Two assistant turns: (1000+1500), (200+350), (5000+6000),
    // (300 + missing=0). The malformed last line is skipped.
    assert_eq!(row.agent, "claude-code");
    assert_eq!(row.input, 2500);
    assert_eq!(row.output, 550);
    assert_eq!(row.cache_read, 11_000);
    assert_eq!(row.cache_creation, 300);
    assert_eq!(
        row.recall_calls, 0,
        "recall_calls is task #30's, not the scraper's"
    );
    assert_eq!(row.started_at, "2026-05-15T09:00:00.000Z");
    assert_eq!(row.ended_at.as_deref(), Some("2026-05-15T09:01:30.000Z"));

    let len = fs::metadata(&file).expect("stat").len() as i64;
    assert_eq!(row.offset, len, "offset advanced to EOF");

    assert_eq!(
        writes_log_skip_count(temp.path()),
        1,
        "exactly one summary writes_log row for the malformed line"
    );
}

#[test]
fn rescan_without_change_does_not_double_count() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let tdir = temp.path().join("transcripts");
    fs::create_dir_all(&tdir).expect("transcripts dir");
    let file = tdir.join("sess-abc.jsonl");
    fs::write(&file, FIXTURE).expect("write fixture");
    enable_metrics(temp.path(), &tdir);

    let first = scrape_and_read(temp.path(), "sess-abc").expect("row");
    // Re-open twice more: the file is unchanged, so the cheap
    // file_len == offset skip must keep counts and offset stable.
    scrape_and_read(temp.path(), "sess-abc");
    let again = scrape_and_read(temp.path(), "sess-abc").expect("row");

    assert_eq!(again.input, first.input);
    assert_eq!(again.output, first.output);
    assert_eq!(again.cache_read, first.cache_read);
    assert_eq!(again.offset, first.offset);
    assert_eq!(
        writes_log_skip_count(temp.path()),
        1,
        "skip is not re-logged on a no-op rescan"
    );
}

#[test]
fn incremental_resume_accumulates_only_the_appended_delta() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let tdir = temp.path().join("transcripts");
    fs::create_dir_all(&tdir).expect("transcripts dir");
    let file = tdir.join("sess-abc.jsonl");
    fs::write(&file, FIXTURE).expect("write fixture");
    enable_metrics(temp.path(), &tdir);

    let base = scrape_and_read(temp.path(), "sess-abc").expect("row");

    append(
        &file,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-05-15T09:05:00.000Z\",\
          \"sessionId\":\"sess-abc\",\"message\":{\"role\":\"assistant\",\
          \"usage\":{\"input_tokens\":700,\"output_tokens\":100}}}\n",
    );

    let after = scrape_and_read(temp.path(), "sess-abc").expect("row");
    assert_eq!(after.input, base.input + 700);
    assert_eq!(after.output, base.output + 100);
    assert_eq!(after.cache_read, base.cache_read, "unchanged keys stay put");
    assert_eq!(
        after.started_at, base.started_at,
        "started_at pinned to earliest"
    );
    assert_eq!(
        after.ended_at.as_deref(),
        Some("2026-05-15T09:05:00.000Z"),
        "ended_at moved forward"
    );
    let len = fs::metadata(&file).expect("stat").len() as i64;
    assert_eq!(after.offset, len);
}

#[test]
fn partial_trailing_line_is_not_consumed_until_completed() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let tdir = temp.path().join("transcripts");
    fs::create_dir_all(&tdir).expect("transcripts dir");
    let file = tdir.join("sess-abc.jsonl");
    fs::write(&file, FIXTURE).expect("write fixture");
    enable_metrics(temp.path(), &tdir);

    let base = scrape_and_read(temp.path(), "sess-abc").expect("row");

    // Half a line, no newline: the session is mid-write.
    append(
        &file,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-05-15T09:06:00.000Z\",\
          \"message\":{\"usage\":{\"input_tokens\":999",
    );
    let mid = scrape_and_read(temp.path(), "sess-abc").expect("row");
    assert_eq!(mid.input, base.input, "partial line not counted");
    assert_eq!(
        mid.offset, base.offset,
        "offset not advanced past a partial line"
    );

    // Complete the line + newline: now it must be counted exactly once.
    append(&file, ",\"output_tokens\":11}}}\n");
    let done = scrape_and_read(temp.path(), "sess-abc").expect("row");
    assert_eq!(done.input, base.input + 999);
    assert_eq!(done.output, base.output + 11);
    let len = fs::metadata(&file).expect("stat").len() as i64;
    assert_eq!(done.offset, len);
}

#[test]
fn session_accounting_kill_switch_stops_the_scrape() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let tdir = temp.path().join("transcripts");
    fs::create_dir_all(&tdir).expect("transcripts dir");
    let file = tdir.join("sess-abc.jsonl");
    fs::write(&file, FIXTURE).expect("write fixture");

    // Master switch on, sub-switch OFF.
    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load");
    cfg.metrics.enabled = true;
    cfg.metrics.session_accounting = false;
    cfg.metrics.claude_transcripts_dir = tdir.to_string_lossy().into_owned();
    cfg.save(&config_path).expect("save");

    assert!(
        scrape_and_read(temp.path(), "sess-abc").is_none(),
        "session_accounting=false must produce no rows"
    );
}

#[test]
fn codex_dir_set_is_a_safe_no_op() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let cdir = temp.path().join("codex");
    fs::create_dir_all(&cdir).expect("codex dir");
    fs::write(cdir.join("whatever.jsonl"), "{}\n").expect("codex file");

    // Metrics on, only the (deferred) Codex dir set, no Claude dir.
    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load");
    cfg.metrics.enabled = true;
    cfg.metrics.session_accounting = true;
    cfg.metrics.codex_transcripts_dir = cdir.to_string_lossy().into_owned();
    cfg.save(&config_path).expect("save");

    // Must not panic and must not invent a session row.
    let ctx = db::open_project(temp.path()).expect("open_project");
    let n: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM session_metrics", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 0, "Codex scraping is deferred; no rows");
}

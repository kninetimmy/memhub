use memhub::commands::{init, narrative};
use memhub::models::NarrativeKind;
use tempfile::tempdir;

#[test]
fn set_inserts_a_new_row_and_show_returns_most_recent() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let first = narrative::set(
        temp.path(),
        NarrativeKind::State,
        "first state",
        "cli:user",
        "cli:user",
    )
    .expect("first set");

    let second = narrative::set(
        temp.path(),
        NarrativeKind::State,
        "second state",
        "cli:user",
        "cli:user",
    )
    .expect("second set");

    assert!(second.id > first.id);

    let shown = narrative::show(temp.path(), NarrativeKind::State)
        .expect("show")
        .expect("at least one row");
    assert_eq!(shown.id, second.id);
    assert_eq!(shown.body, "second state");
}

#[test]
fn show_returns_none_when_no_rows() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let shown = narrative::show(temp.path(), NarrativeKind::Arch).expect("show");
    assert!(shown.is_none());
}

#[test]
fn history_orders_newest_first_and_respects_limit() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    for n in 0..3 {
        narrative::set(
            temp.path(),
            NarrativeKind::Arch,
            &format!("arch v{n}"),
            "cli:user",
            "cli:user",
        )
        .expect("set");
    }

    let all = narrative::history(temp.path(), NarrativeKind::Arch, 10).expect("history all");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].body, "arch v2");
    assert_eq!(all[2].body, "arch v0");

    let limited = narrative::history(temp.path(), NarrativeKind::Arch, 2).expect("history limit");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].body, "arch v2");
    assert_eq!(limited[1].body, "arch v1");
}

#[test]
fn state_and_arch_are_independent_tables() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    narrative::set(
        temp.path(),
        NarrativeKind::State,
        "state body",
        "cli:user",
        "cli:user",
    )
    .expect("state set");

    let arch_shown = narrative::show(temp.path(), NarrativeKind::Arch).expect("arch show");
    assert!(arch_shown.is_none());

    let state_shown = narrative::show(temp.path(), NarrativeKind::State)
        .expect("state show")
        .expect("state row");
    assert_eq!(state_shown.body, "state body");
}

#[test]
fn set_rejects_empty_body() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let err = narrative::set(
        temp.path(),
        NarrativeKind::State,
        "   \n  ",
        "cli:user",
        "cli:user",
    )
    .expect_err("empty body should fail");

    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn set_rejects_oversized_body() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let huge = "a".repeat(narrative::MAX_BODY_LEN + 1);
    let err = narrative::set(
        temp.path(),
        NarrativeKind::State,
        &huge,
        "cli:user",
        "cli:user",
    )
    .expect_err("oversized body should fail");

    assert!(err.to_string().contains("characters or fewer"));
}

#[test]
fn history_rejects_zero_limit() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let err =
        narrative::history(temp.path(), NarrativeKind::State, 0).expect_err("zero limit fails");
    assert!(err.to_string().contains("greater than zero"));
}

#[test]
fn set_records_writes_log_entry() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    narrative::set(
        temp.path(),
        NarrativeKind::State,
        "audit me",
        "test:actor",
        "test:actor",
    )
    .expect("set");

    let conn = rusqlite::Connection::open(temp.path().join(".memhub").join("project.sqlite"))
        .expect("open db");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM writes_log
             WHERE actor = ?1 AND table_name = ?2 AND action = 'insert'",
            rusqlite::params!["test:actor", "project_state"],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

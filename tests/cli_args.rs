//! Cheap `Cli::try_parse_from` smoke test over the clap argument surface
//! (issue #15 / F1). The clap CLI surface had zero test coverage before
//! this (N24) — a flag rename or omission shipped silently. This is
//! parse-only: it never opens a DB or touches the filesystem, so it is
//! milliseconds-cost and scoped to the `--json` flags this change adds
//! (`status`, `init`, `fact list`, `decision list`, `command list`), plus
//! a regression guard that `doc ls --json` still parses after its output
//! shape moved from a bare array to `{"docs": [...]}`.
//!
//! A broader parity-gate matrix over every `memhub ...` line in the
//! skill templates is tracked separately (N24/P13 -> Q43, not yet
//! resolved) and is out of scope here.

use clap::Parser;
use memhub::cli::{
    AuditCommand, Cli, CommandCommand, DecisionCommand, DocCommand, FactCommand, TopLevelCommand,
};

fn parse(args: &[&str]) -> TopLevelCommand {
    let mut full = vec!["memhub"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full)
        .unwrap_or_else(|e| panic!("failed to parse {args:?}: {e}"))
        .command
}

#[test]
fn status_json_flag_parses() {
    match parse(&["status", "--json"]) {
        TopLevelCommand::Status { json } => assert!(json),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn status_without_json_defaults_to_false() {
    match parse(&["status"]) {
        TopLevelCommand::Status { json } => assert!(!json),
        other => panic!("expected Status, got {other:?}"),
    }
}

/// Wave 1·A / issue #21: `memhub doctor [--json] [--strict]`.
#[test]
fn doctor_json_and_strict_flags_parse() {
    match parse(&["doctor", "--json", "--strict"]) {
        TopLevelCommand::Doctor { json, strict } => {
            assert!(json);
            assert!(strict);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
}

#[test]
fn doctor_without_flags_defaults_to_false() {
    match parse(&["doctor"]) {
        TopLevelCommand::Doctor { json, strict } => {
            assert!(!json);
            assert!(!strict);
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
}

#[test]
fn init_json_flag_parses() {
    match parse(&["init", "--json"]) {
        TopLevelCommand::Init { from_backup, json } => {
            assert!(json);
            assert!(from_backup.is_none());
        }
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn init_from_backup_combines_with_json() {
    match parse(&["init", "--from-backup", "backup.json", "--json"]) {
        TopLevelCommand::Init { from_backup, json } => {
            assert!(json);
            assert_eq!(
                from_backup.expect("from_backup").to_str(),
                Some("backup.json")
            );
        }
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn fact_list_json_flag_parses() {
    match parse(&["fact", "list", "--json"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::List { json } => assert!(json),
            other => panic!("expected FactCommand::List, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
    }
}

#[test]
fn fact_list_without_json_defaults_to_false() {
    match parse(&["fact", "list"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::List { json } => assert!(!json),
            other => panic!("expected FactCommand::List, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
    }
}

#[test]
fn decision_list_json_flag_parses() {
    match parse(&["decision", "list", "--json"]) {
        TopLevelCommand::Decision { command } => match command {
            DecisionCommand::List { json } => assert!(json),
            other => panic!("expected DecisionCommand::List, got {other:?}"),
        },
        other => panic!("expected Decision, got {other:?}"),
    }
}

#[test]
fn command_list_json_flag_parses() {
    match parse(&["command", "list", "--json"]) {
        TopLevelCommand::Command { command } => match command {
            CommandCommand::List { json } => assert!(json),
            other => panic!("expected CommandCommand::List, got {other:?}"),
        },
        other => panic!("expected Command, got {other:?}"),
    }
}

/// Regression guard for the F1 doc-ls migration: the flag itself is
/// unchanged (only the runtime JSON shape moved from a bare array to
/// `{"docs": [...]}`), so parsing must be unaffected.
#[test]
fn doc_ls_json_flag_still_parses() {
    match parse(&["doc", "ls", "--json"]) {
        TopLevelCommand::Doc { command } => match command {
            DocCommand::Ls { global, json } => {
                assert!(json);
                assert!(!global);
            }
            other => panic!("expected DocCommand::Ls, got {other:?}"),
        },
        other => panic!("expected Doc, got {other:?}"),
    }
}

/// Wave 2 C5 / issue #32: `memhub audit md [--json] [--strict]` (N24
/// precedent).
#[test]
fn audit_md_json_and_strict_flags_parse() {
    match parse(&["audit", "md", "--json", "--strict"]) {
        TopLevelCommand::Audit { command } => match command {
            AuditCommand::Md { json, strict } => {
                assert!(json);
                assert!(strict);
            }
        },
        other => panic!("expected Audit, got {other:?}"),
    }
}

#[test]
fn audit_md_without_flags_defaults_to_false() {
    match parse(&["audit", "md"]) {
        TopLevelCommand::Audit { command } => match command {
            AuditCommand::Md { json, strict } => {
                assert!(!json);
                assert!(!strict);
            }
        },
        other => panic!("expected Audit, got {other:?}"),
    }
}

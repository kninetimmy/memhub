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
#[cfg(feature = "metrics")]
use memhub::cli::MetricsCommand;
use memhub::cli::{
    AuditCommand, Cli, CommandCommand, DecisionCommand, DocCommand, FactCommand, TopLevelCommand,
};
use memhub::code_index::locate::DEFAULT_LOCATE_LIMIT;

fn parse(args: &[&str]) -> TopLevelCommand {
    let mut full = vec!["memhub"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full)
        .unwrap_or_else(|e| panic!("failed to parse {args:?}: {e}"))
        .command
}

#[cfg(not(feature = "metrics"))]
#[test]
fn metrics_command_is_absent_from_hibernated_build() {
    assert!(Cli::try_parse_from(["memhub", "metrics", "status"]).is_err());
}

#[cfg(feature = "metrics")]
#[test]
fn metrics_command_returns_in_reactivated_build() {
    match parse(&["metrics", "status"]) {
        TopLevelCommand::Metrics {
            command: MetricsCommand::Status { json },
        } => assert!(!json),
        other => panic!("expected Metrics status, got {other:?}"),
    }
}

#[cfg(not(feature = "viz"))]
#[test]
fn viz_command_is_absent_from_hibernated_build() {
    assert!(Cli::try_parse_from(["memhub", "viz"]).is_err());
}

#[cfg(feature = "viz")]
#[test]
fn viz_command_returns_in_reactivated_build() {
    match parse(&["viz"]) {
        TopLevelCommand::Viz { host, port, open } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 0);
            assert!(!open);
        }
        other => panic!("expected Viz, got {other:?}"),
    }
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

#[test]
fn render_actor_flag_parses() {
    match parse(&["render", "--actor", "codex:wrap-up"]) {
        TopLevelCommand::Render { actor } => {
            assert_eq!(actor.as_deref(), Some("codex:wrap-up"));
        }
        other => panic!("expected Render, got {other:?}"),
    }
}

#[test]
fn render_without_actor_defaults_to_none() {
    match parse(&["render"]) {
        TopLevelCommand::Render { actor } => assert!(actor.is_none()),
        other => panic!("expected Render, got {other:?}"),
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

/// Wave 6 W4 / issue #97: `memhub fact add <key> <value> [--kind <k>]`.
#[test]
fn fact_add_kind_flag_parses() {
    match parse(&["fact", "add", "my-key", "my-value", "--kind", "gotcha"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Add { key, value, kind, .. } => {
                assert_eq!(key, "my-key");
                assert_eq!(value, "my-value");
                assert_eq!(kind.as_deref(), Some("gotcha"));
            }
            other => panic!("expected FactCommand::Add, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
    }
}

#[test]
fn fact_add_without_kind_defaults_to_none() {
    match parse(&["fact", "add", "my-key", "my-value"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Add { kind, .. } => assert!(kind.is_none()),
            other => panic!("expected FactCommand::Add, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
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

/// Wave 3 L1 / issue #41: `memhub fact verify <id|key> [--json] [--actor]`.
#[test]
fn fact_verify_parses_ident_json_and_actor() {
    match parse(&["fact", "verify", "my-key", "--json", "--actor", "cli:script"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Verify { ident, json, actor } => {
                assert_eq!(ident, "my-key");
                assert!(json);
                assert_eq!(actor.as_deref(), Some("cli:script"));
            }
            other => panic!("expected FactCommand::Verify, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
    }
}

#[test]
fn fact_verify_without_flags_defaults() {
    match parse(&["fact", "verify", "42"]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Verify { ident, json, actor } => {
                assert_eq!(ident, "42");
                assert!(!json);
                assert!(actor.is_none());
            }
            other => panic!("expected FactCommand::Verify, got {other:?}"),
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

/// Wave 3 L3 / issue #46: `memhub fact supersede <old> --by <new>`. Both
/// sides accept a numeric id or an exact key (parsed as strings).
#[test]
fn fact_supersede_parses_old_and_by() {
    match parse(&[
        "fact",
        "supersede",
        "old-key",
        "--by",
        "new-key",
        "--actor",
        "cli:script",
    ]) {
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Supersede {
                old,
                by,
                json,
                actor,
            } => {
                assert_eq!(old, "old-key");
                assert_eq!(by, "new-key");
                assert!(!json);
                assert_eq!(actor.as_deref(), Some("cli:script"));
            }
            other => panic!("expected FactCommand::Supersede, got {other:?}"),
        },
        other => panic!("expected Fact, got {other:?}"),
    }
}

/// `memhub decision supersede <old> --by <new>` — decisions have no natural
/// key, so both ids parse as integers.
#[test]
fn decision_supersede_parses_numeric_ids() {
    match parse(&["decision", "supersede", "3", "--by", "7"]) {
        TopLevelCommand::Decision { command } => match command {
            DecisionCommand::Supersede { old, by, .. } => {
                assert_eq!(old, 3);
                assert_eq!(by, 7);
            }
            other => panic!("expected DecisionCommand::Supersede, got {other:?}"),
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

/// Wave 6 W1+W2 / issue #95: `memhub wrapup-policy [--json]`.
#[test]
fn wrapup_policy_json_flag_parses() {
    match parse(&["wrapup-policy", "--json"]) {
        TopLevelCommand::WrapupPolicy { json } => assert!(json),
        other => panic!("expected WrapupPolicy, got {other:?}"),
    }
}

#[test]
fn wrapup_policy_without_json_defaults_to_false() {
    match parse(&["wrapup-policy"]) {
        TopLevelCommand::WrapupPolicy { json } => assert!(!json),
        other => panic!("expected WrapupPolicy, got {other:?}"),
    }
}

/// Wave 4 R5 / issue #67: `memhub locate <query> [--limit N] [--rerank]
/// [--no-refresh] [--json]`.
#[test]
fn locate_flags_parse() {
    match parse(&[
        "locate",
        "parse manifest",
        "--limit",
        "3",
        "--rerank",
        "--no-refresh",
        "--json",
    ]) {
        TopLevelCommand::Locate {
            query,
            limit,
            rerank,
            no_refresh,
            json,
        } => {
            assert_eq!(query, "parse manifest");
            assert_eq!(limit, 3);
            assert!(rerank);
            assert!(no_refresh);
            assert!(json);
        }
        other => panic!("expected Locate, got {other:?}"),
    }
}

#[test]
fn locate_without_flags_defaults() {
    match parse(&["locate", "parse manifest"]) {
        TopLevelCommand::Locate {
            query,
            limit,
            rerank,
            no_refresh,
            json,
        } => {
            assert_eq!(query, "parse manifest");
            assert_eq!(limit, DEFAULT_LOCATE_LIMIT);
            assert!(!rerank);
            assert!(!no_refresh, "--no-refresh must default to false");
            assert!(!json);
        }
        other => panic!("expected Locate, got {other:?}"),
    }
}

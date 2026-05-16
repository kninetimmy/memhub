use std::fs;

use memhub::commands::init;
use memhub::config::ProjectConfig;
use memhub::db;
use tempfile::tempdir;

/// On a fresh clone, the tracked `.memhub/config.example.toml` is the
/// only thing under `.memhub/` until the first `memhub` call. Init must
/// pick the example up and seed `.memhub/config.toml` from it, so every
/// machine starts with the canonical baseline instead of the code
/// defaults (which may drift from the canonical settings).
#[test]
fn init_seeds_local_config_from_tracked_example_when_present() {
    let temp = tempdir().expect("tempdir");
    let memhub_dir = temp.path().join(".memhub");
    fs::create_dir_all(&memhub_dir).expect("create .memhub");

    let example = r#"project_name = "from-example"
auto_sync_md = true
log_level = "debug"

[deny_list]
patterns = ["custom-secret.*"]

[integrations.k9]
enabled = false
agent_docs_path = "agent_docs"

[render]
output_dir = ".memhub/rendered"

[retrieval]
mode = "hybrid"
default_max_results = 9
accepted_only_by_default = false
include_stale_by_default = false

[retrieval.scoring]
fts_weight = 0.3
vector_weight = 0.7
stale_penalty = 0.4
min_vector_score = 0.6
"#;
    fs::write(memhub_dir.join("config.example.toml"), example).expect("write example");

    init::run(temp.path()).expect("init succeeds");

    let local_config_path = memhub_dir.join("config.toml");
    let raw = fs::read_to_string(&local_config_path).expect("read local config");
    let config: ProjectConfig = toml::from_str(&raw).expect("parse local config");

    assert_eq!(config.project_name, "from-example");
    assert!(config.auto_sync_md);
    assert_eq!(config.log_level, "debug");
    assert_eq!(config.retrieval.default_max_results, 9);
    assert_eq!(config.retrieval.scoring.fts_weight, 0.3);
    assert_eq!(config.retrieval.scoring.vector_weight, 0.7);
    assert_eq!(
        config.deny_list.patterns,
        vec!["custom-secret.*".to_string()]
    );
}

/// Without an example file, init falls back to the code-defined
/// defaults seeded with the repo directory name. This is the
/// pre-step-6 behavior and must continue to work for repos that
/// haven't adopted the example pattern.
#[test]
fn init_falls_back_to_code_defaults_when_no_example_present() {
    let temp = tempdir().expect("tempdir");

    init::run(temp.path()).expect("init succeeds");

    let local_config_path = temp.path().join(".memhub").join("config.toml");
    let config = ProjectConfig::load(&local_config_path).expect("load local config");

    let repo_name = temp
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("tempdir basename");
    assert_eq!(config.project_name, repo_name);
    assert!(!config.auto_sync_md);
    assert_eq!(config.log_level, "info");
}

/// A corrupt example file (invalid TOML, or TOML that doesn't match the
/// `ProjectConfig` shape) must surface as a clean error rather than
/// being copied verbatim and then erroring on the next config load.
#[test]
fn init_rejects_corrupt_example_without_writing_local_config() {
    let temp = tempdir().expect("tempdir");
    let memhub_dir = temp.path().join(".memhub");
    fs::create_dir_all(&memhub_dir).expect("create .memhub");

    // Valid TOML but wrong shape (missing required fields).
    fs::write(memhub_dir.join("config.example.toml"), "wat = true\n").expect("write bad example");

    let result = init::run(temp.path());
    assert!(result.is_err(), "init should reject a malformed example");

    let local_config_path = memhub_dir.join("config.toml");
    assert!(
        !local_config_path.exists(),
        "no local config should be written when the example is invalid"
    );
}

/// If `.memhub/config.toml` already exists, the example must NOT be
/// reconsulted on subsequent invocations. Per-machine edits to the
/// local config stay sticky; you have to delete the local config to
/// re-seed from the example.
#[test]
fn existing_local_config_is_never_overwritten_by_example() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let memhub_dir = temp.path().join(".memhub");
    let local_config_path = memhub_dir.join("config.toml");
    let mut local = ProjectConfig::load(&local_config_path).expect("load local");
    local.log_level = "trace".to_string();
    local.save(&local_config_path).expect("save local edit");

    // Drop a different example in place after the local config exists.
    fs::write(
        memhub_dir.join("config.example.toml"),
        r#"project_name = "should-be-ignored"
auto_sync_md = false
log_level = "warn"
"#,
    )
    .expect("write example");

    // Subsequent open_project must keep the local edit, not re-seed from example.
    let _ = db::open_project(temp.path()).expect("open project");
    let after = ProjectConfig::load(&local_config_path).expect("reload local");
    assert_eq!(after.log_level, "trace");
    assert_ne!(after.project_name, "should-be-ignored");
}

/// The tracked `.memhub/config.example.toml` is the canonical baseline
/// for every machine. Parse it directly and pin the `[metrics]` block
/// (step 2/10 of decision 74) so a future edit cannot silently flip a
/// default — e.g. shipping `enabled = true` to every install — without
/// this test catching it.
#[test]
fn tracked_example_config_pins_metrics_defaults() {
    let raw = fs::read_to_string(".memhub/config.example.toml")
        .expect("read tracked .memhub/config.example.toml");
    let config: ProjectConfig = toml::from_str(&raw).expect("parse tracked example");

    assert!(!config.metrics.enabled);
    assert!(config.metrics.recall_proxy);
    assert!(config.metrics.session_accounting);
    assert_eq!(config.metrics.claude_transcripts_dir, "");
    assert_eq!(config.metrics.codex_transcripts_dir, "");
    assert_eq!(config.metrics.tokenizer, "tiktoken-cl100k");
    assert_eq!(config.metrics.retention_days, 90);
}

/// An install that hasn't pulled the new example (no `[metrics]` block
/// in its local `.memhub/config.toml`) must still load cleanly with
/// metrics off. Guards against existing repos breaking on the first
/// run after the decision-74 build.
#[test]
fn pre_metrics_local_config_loads_with_metrics_off() {
    let temp = tempdir().expect("tempdir");
    let memhub_dir = temp.path().join(".memhub");
    fs::create_dir_all(&memhub_dir).expect("create .memhub");

    let legacy = r#"project_name = "pre-metrics"
auto_sync_md = false
log_level = "info"
"#;
    let local = memhub_dir.join("config.toml");
    fs::write(&local, legacy).expect("write legacy config");

    let config = ProjectConfig::load(&local).expect("load legacy config");
    assert!(!config.metrics.enabled);
    assert!(config.metrics.recall_proxy);
    assert_eq!(config.metrics.tokenizer, "tiktoken-cl100k");
    assert_eq!(config.metrics.retention_days, 90);
}

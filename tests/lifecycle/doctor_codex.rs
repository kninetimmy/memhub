//! Inspect Codex registration and installed skill presence through the real
//! CLI, including human/JSON output and doctor's normal/strict exit contract.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

fn run_cli(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub")
}

#[test]
fn doctor_codex_project_only_and_disabled_override_json_and_exit_codes() {
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    for args in [["init"], ["render"]] {
        let output = run_cli(repo.path(), home.path(), &args);
        assert!(output.status.success(), "{output:?}");
    }
    fs::create_dir_all(repo.path().join(".codex")).expect("project config directory");
    let project_config = repo.path().join(".codex/config.toml");
    let user_config = home.path().join(".codex/config.toml");
    let server = "[mcp_servers.memhub]\ncommand = 'memhub'\nargs = ['serve']\n";
    fs::write(&project_config, server).expect("project config");

    for disabled in [false, true] {
        if disabled {
            fs::create_dir_all(home.path().join(".codex")).expect("user config directory");
            fs::write(&user_config, server).expect("user config");
            fs::write(&project_config, "[mcp_servers.memhub]\nenabled = false\n")
                .expect("project override");
        }
        for strict in [false, true] {
            let args = if strict {
                vec!["doctor", "--json", "--strict"]
            } else {
                vec!["doctor", "--json"]
            };
            let output = run_cli(repo.path(), home.path(), &args);
            let body: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|e| panic!("invalid doctor JSON: {e}; {output:?}"));
            let report = &body["doctor"];
            let check = report["checks"]
                .as_array()
                .expect("checks")
                .iter()
                .find(|check| check["id"] == "mcp_registration_codex")
                .expect("Codex registration check");
            assert_eq!(check["status"], if disabled { "warn" } else { "ok" });
            assert!(
                check["message"]
                    .as_str()
                    .unwrap()
                    .contains("repository-root")
            );
            assert!(
                check["detail"]
                    .as_str()
                    .unwrap()
                    .contains("trusted project")
            );
            assert_eq!(report["counts"]["error"], 0, "{body}");
            assert_eq!(report["counts"]["warn"], usize::from(disabled), "{body}");
            let expected_exit = i32::from(disabled && strict);
            assert_eq!(output.status.code(), Some(expected_exit), "{body}");
            assert_eq!(report["exit_code"], expected_exit, "{body}");
        }
    }
    assert_eq!(fs::read_to_string(user_config).unwrap(), server);
    assert_eq!(
        fs::read_to_string(project_config).unwrap(),
        "[mcp_servers.memhub]\nenabled = false\n"
    );
}

#[test]
fn doctor_installed_skills_human_json_and_exit_codes_outside_source_checkout() {
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    for args in [["init"], ["render"]] {
        let output = run_cli(repo.path(), home.path(), &args);
        assert!(output.status.success(), "{output:?}");
    }
    // This project has no source templates. Register MCP separately so every
    // warning comes from the empty skill install directories.
    fs::create_dir(repo.path().join(".codex")).unwrap();
    fs::write(
        repo.path().join(".codex/config.toml"),
        "[mcp_servers.memhub]\ncommand = 'memhub'\nargs = ['serve']\n",
    )
    .unwrap();
    fs::write(
        repo.path().join(".mcp.json"),
        r#"{"mcpServers":{"memhub":{"command":"memhub","args":["serve"]}}}"#,
    )
    .unwrap();
    fs::write(
        repo.path().join("opencode.json"),
        r#"{"mcp":{"servers":{"memhub":{"command":"memhub"}}}}"#,
    )
    .unwrap();
    let surfaces = [
        ("installed_skills_claude", ".claude/commands", false),
        ("installed_skills_codex", ".codex/skills", true),
        ("installed_skills_opencode", ".config/opencode/skills", true),
        (
            "installed_commands_opencode",
            ".config/opencode/commands",
            false,
        ),
    ];
    for (_, relative, _) in surfaces {
        fs::create_dir_all(home.path().join(relative)).unwrap();
    }
    for strict in [false, true] {
        let mut args = vec!["doctor"];
        if strict {
            args.push("--strict");
        }
        let human = run_cli(repo.path(), home.path(), &args);
        let text = String::from_utf8(human.stdout).unwrap();
        args.push("--json");
        let json = run_cli(repo.path(), home.path(), &args);
        let body: Value = serde_json::from_slice(&json.stdout).unwrap();
        let report = &body["doctor"];
        assert_eq!(report["counts"]["warn"], 4, "{body}");
        assert_eq!(report["counts"]["error"], 0, "{body}");
        assert_eq!(report["exit_code"], i32::from(strict));
        assert_eq!(human.status.code(), Some(i32::from(strict)));
        assert_eq!(json.status.code(), Some(i32::from(strict)));
        for (id, relative, nested) in surfaces {
            let check = report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["id"] == id)
                .unwrap();
            assert_eq!(check["status"], "warn");
            let message = check["message"].as_str().unwrap();
            assert!(message.contains("recall:"));
            let target = home.path().join(relative);
            let entrypoint = if nested {
                target.join("recall").join("SKILL.md")
            } else {
                target.join("recall.md")
            };
            assert!(
                message.contains(&entrypoint.display().to_string()),
                "{message}"
            );
            assert_eq!(message.contains("metrics:"), cfg!(feature = "metrics"));
            assert!(!message.contains("viz:"));
            assert!(message.contains("memhub upgrade"));
            assert!(text.contains(message), "{text}");
            assert!(text.contains(check["detail"].as_str().unwrap()), "{text}");
            assert_eq!(fs::read_dir(home.path().join(relative)).unwrap().count(), 0);
        }
    }
    assert!(!home.path().join(".memhub/install-manifest.json").exists());
}

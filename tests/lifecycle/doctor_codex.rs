//! Issue #415: inspect project Codex registration through the real CLI,
//! including disabled overrides and doctor's normal/strict exit contract.

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

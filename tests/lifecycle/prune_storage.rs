use std::process::Command;

#[test]
fn windows_ci_refuses_missing_or_unusable_controlled_storage() {
    let missing = tempfile::tempdir().unwrap().path().join("does-not-exist");
    for root in [None, Some(missing.as_os_str()), Some("relative".as_ref())] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "prune::scan_uses_missing_config_fallback_without_creating_a_live_config",
                "--nocapture",
            ])
            .env("CI", "true")
            .env_remove("MEMHUB_TEST_PRUNE_ROOT");
        if let Some(root) = root {
            command.env("MEMHUB_TEST_PRUNE_ROOT", root);
        }
        let out = command.output().unwrap();
        assert!(!out.status.success(), "{out:?}");
        let error = String::from_utf8_lossy(&out.stderr);
        let expected = match root {
            None => "Windows CI must run scripts/test-windows.ps1",
            Some(root) if root == "relative" => "prune fixture root must be absolute",
            Some(_) => "create prune fixture on controlled storage",
        };
        assert!(error.contains(expected), "{out:?}");
    }
}

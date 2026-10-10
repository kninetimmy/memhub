//! Issue #286: `memhub hook session-start` and the SessionStart hook entry
//! `memhub upgrade` merges into `~/.claude/settings.json` and
//! `~/.codex/hooks.json`.
//!
//! Every test points `HOME`/`USERPROFILE` at a throwaway dir: the in-process
//! installer test takes `support::env_lock()`, and the binary tests set the
//! child's environment only. Nothing here reads or writes the real home.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use memhub::commands::hook::{HookInstall, HookStatus, install_session_hooks};
use memhub::commands::init;
use serde_json::{Value, json};
use tempfile::tempdir;

const CMD: &str = "memhub hook session-start";

fn memhub(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub")
}

/// Every file under `root` with its bytes and mtime, to prove a run
/// created, removed, or modified nothing.
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let mtime = entry.metadata().ok().and_then(|m| m.modified().ok());
                out.insert(path.clone(), (std::fs::read(&path).expect("read"), mtime));
            }
        }
    }
    out
}

fn assert_silent_noop(cwd: &Path, home: &Path, watched: &Path, why: &str) {
    let before = tree(watched);
    let out = memhub(cwd, home, &["hook", "session-start"]);
    assert!(out.status.success(), "{why}: exit {:?}", out.status);
    assert!(out.stdout.is_empty(), "{why}: stdout must be empty");
    assert_eq!(tree(watched), before, "{why}: no file may change");
}

#[test]
fn hook_session_start_prints_project_md_or_nothing() {
    let root = tempdir().expect("tempdir");
    let home = root.path().join("home");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&repo).expect("repo");
    init::run(&repo).expect("init");

    let rendered = repo.join(".memhub").join("rendered");
    std::fs::create_dir_all(&rendered).expect("rendered dir");
    // CRLF, non-ASCII, and no trailing newline: must come through untouched.
    let frame = "# Frame\r\n\nCurrently building: caf\u{e9} \u{2014} hooks".as_bytes();
    std::fs::write(rendered.join("PROJECT.md"), frame).expect("write PROJECT.md");

    // Criterion 1: repo root and a nested subdirectory, byte-for-byte.
    let sub = repo.join("src").join("deep");
    std::fs::create_dir_all(&sub).expect("subdir");
    for cwd in [&repo, &sub] {
        let before = tree(root.path());
        let out = memhub(cwd, &home, &["hook", "session-start"]);
        assert!(out.status.success(), "exit {:?}", out.status);
        assert_eq!(out.stdout, frame, "stdout must be PROJECT.md byte-for-byte");
        assert_eq!(
            tree(root.path()),
            before,
            "printing must not touch any file"
        );
    }

    // Criterion 2: unreadable config (PROJECT.md still present, so silence
    // is due to the config alone).
    let config = repo.join(".memhub").join("config.toml");
    let good_config = std::fs::read(&config).expect("read config");
    std::fs::write(&config, b"this is = = not toml [[[").expect("corrupt config");
    assert_silent_noop(&repo, &home, root.path(), "unreadable config");
    std::fs::write(&config, &good_config).expect("restore config");

    // Criterion 2: no rendered PROJECT.md.
    std::fs::remove_file(rendered.join("PROJECT.md")).expect("rm PROJECT.md");
    assert_silent_noop(&sub, &home, root.path(), "no PROJECT.md");

    // Criterion 2: outside any memhub repo. On a developer Windows machine
    // the temp dir sits inside the real profile, so the upward walk can
    // reach a real `~/.memhub`; it is only ever read, never written.
    let outside = root.path().join("not-a-repo");
    std::fs::create_dir_all(&outside).expect("outside dir");
    assert_silent_noop(&outside, &home, root.path(), "outside any repo");
}

#[test]
fn hook_subcommand_is_hidden_from_help() {
    let home = tempdir().expect("home");
    let out = memhub(home.path(), home.path(), &["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("upgrade"),
        "sanity: help lists commands: {help}"
    );
    assert!(
        !help.lines().any(|l| l.trim_start().starts_with("hook")),
        "`hook` must not be listed in --help: {help}"
    );
}

fn memhub_groups(file: &Path) -> Vec<Value> {
    let v: Value = serde_json::from_slice(&std::fs::read(file).expect("read")).expect("json");
    v["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart array")
        .iter()
        .filter(|g| {
            g["hooks"]
                .as_array()
                .is_some_and(|hs| hs.iter().any(|h| h["command"] == CMD))
        })
        .cloned()
        .collect()
}

fn status_of(reports: &[HookInstall], agent: &str) -> HookInstall {
    reports
        .iter()
        .find(|r| r.agent == agent)
        .unwrap_or_else(|| panic!("no {agent} row in {reports:?}"))
        .clone()
}

/// Point the in-process installer at `home`. Callers hold `env_lock()`.
fn set_home(home: &Path) {
    unsafe {
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
    }
}

#[test]
fn install_merges_preserves_dedupes_and_respects_removal() {
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let claude = home.path().join(".claude").join("settings.json");
    let codex = home.path().join(".codex").join("hooks.json");
    let marker = home.path().join(".memhub").join("installed-hooks.json");
    // Both agents are set up (their dirs exist); the files do not exist yet.
    std::fs::create_dir_all(claude.parent().unwrap()).expect("mk .claude");
    std::fs::create_dir_all(codex.parent().unwrap()).expect("mk .codex");

    // Criterion 8 (dry run): reports both additions, writes nothing.
    let dry = install_session_hooks(true);
    assert_eq!(status_of(&dry, "claude").status, HookStatus::WouldAdd);
    assert_eq!(status_of(&dry, "codex").status, HookStatus::WouldAdd);
    assert!(!claude.exists() && !codex.exists() && !marker.exists());

    // Seed a Claude settings file with unrelated settings and hooks,
    // including a SessionStart group with a different matcher.
    let seeded = json!({
        "model": "opus",
        "permissions": {"allow": ["Bash(ls:*)"], "deny": []},
        "env": {"FOO": "1"},
        "hooks": {
            "SessionStart": [
                {"matcher": "startup", "hooks": [{"type": "command", "command": "other-tool start"}]}
            ],
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "other-tool prompt", "timeout": 5}]}
            ]
        }
    });
    std::fs::write(&claude, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed");

    // Criteria 4 + 9: added to both; the missing Codex file is created and
    // its row tells the user to approve the hook in /hooks.
    let first = install_session_hooks(false);
    assert_eq!(status_of(&first, "claude").status, HookStatus::Added);
    let codex_row = status_of(&first, "codex");
    assert_eq!(codex_row.status, HookStatus::Added);
    assert!(
        codex_row.line().contains("/hooks"),
        "codex row must point at /hooks: {}",
        codex_row.line()
    );
    // #298 criterion 1: only the Codex handler carries the output limit.
    assert_eq!(
        memhub_groups(&claude),
        vec![json!({
            "matcher": "startup|resume|clear|compact",
            "hooks": [{"type": "command", "command": CMD, "timeout": 30}]
        })]
    );
    assert_eq!(
        memhub_groups(&codex),
        vec![json!({
            "matcher": "startup|resume",
            "hooks": [{
                "type": "command", "command": CMD, "timeout": 30,
                "additionalContextLimit": 4000
            }]
        })]
    );

    // Criterion 5: every prior key, value, and hook entry survives.
    let after: Value = serde_json::from_slice(&std::fs::read(&claude).unwrap()).unwrap();
    for key in ["model", "permissions", "env"] {
        assert_eq!(after[key], seeded[key], "{key} changed");
    }
    assert_eq!(
        after["hooks"]["UserPromptSubmit"],
        seeded["hooks"]["UserPromptSubmit"]
    );
    assert_eq!(
        after["hooks"]["SessionStart"][0], seeded["hooks"]["SessionStart"][0],
        "the other SessionStart group must survive"
    );
    assert_eq!(after["hooks"]["SessionStart"].as_array().unwrap().len(), 2);

    // Criterion 5: a second run adds nothing and rewrites nothing.
    let (claude_bytes, codex_bytes) = (
        std::fs::read(&claude).unwrap(),
        std::fs::read(&codex).unwrap(),
    );
    let again = install_session_hooks(false);
    assert!(
        again.iter().all(|r| r.status == HookStatus::AlreadyPresent),
        "{again:?}"
    );
    assert_eq!(std::fs::read(&claude).unwrap(), claude_bytes);
    assert_eq!(std::fs::read(&codex).unwrap(), codex_bytes);

    // Criterion 7: the user deletes memhub's Claude entry; it stays out.
    std::fs::write(&claude, serde_json::to_vec_pretty(&seeded).unwrap()).expect("remove entry");
    let removed_bytes = std::fs::read(&claude).unwrap();
    let later = install_session_hooks(false);
    let row = status_of(&later, "claude");
    assert_eq!(row.status, HookStatus::LeftOut);
    assert!(row.line().contains("left out"), "{}", row.line());
    // #298 criterion 7: the user may have added the entry by hand, so the
    // row must not claim memhub added it.
    assert!(!row.line().contains("added"), "{}", row.line());
    assert_eq!(std::fs::read(&claude).unwrap(), removed_bytes);
    assert_eq!(install_session_hooks(true)[0].status, HookStatus::LeftOut);

    // Criterion 6: an invalid file is left byte-for-byte and reported; the
    // other file is still processed.
    std::fs::write(&codex, b"{ \"hooks\": not json").expect("corrupt codex");
    let broken = install_session_hooks(false);
    let row = status_of(&broken, "codex");
    assert_eq!(row.status, HookStatus::NotInstalled);
    assert!(row.line().contains("not installed"), "{}", row.line());
    assert_eq!(std::fs::read(&codex).unwrap(), b"{ \"hooks\": not json");
    assert_eq!(status_of(&broken, "claude").status, HookStatus::LeftOut);

    // Valid JSON of the wrong shape is not ours to reshape either.
    std::fs::write(&codex, b"{\"hooks\": []}").expect("wrong shape");
    assert_eq!(
        status_of(&install_session_hooks(false), "codex").status,
        HookStatus::NotInstalled
    );
    assert_eq!(std::fs::read(&codex).unwrap(), b"{\"hooks\": []}");
}

fn upgrade_env(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .env("CARGO_HOME", home.join(".cargo"))
        .env_remove("CARGO_INSTALL_ROOT")
        .env("MEMHUB_LOG", "off");
}

fn upgrade(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_memhub"));
    cmd.arg("upgrade").args(args).current_dir(cwd);
    upgrade_env(&mut cmd, home);
    let out = cmd.output().expect("spawn upgrade");
    assert!(
        out.status.success(),
        "upgrade {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json_hooks(out: &Output) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().expect("json line");
    let v: Value = serde_json::from_str(first).expect("upgrade json");
    v["hooks"]
        .as_array()
        .expect("hooks array")
        .iter()
        .map(|r| {
            (
                r["agent"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn upgrade_report_covers_no_hooks_dry_run_and_install() {
    // The finish phase (the part that installs hooks) and the dry-run
    // preview, driven through the real binary with a throwaway HOME.
    let home = tempdir().expect("home");
    let work = tempdir().expect("work dir");
    let claude = home.path().join(".claude").join("settings.json");
    let codex = home.path().join(".codex").join("hooks.json");
    let pair = |a: &str, b: &str| vec![("claude".into(), a.into()), ("codex".into(), b.into())];
    std::fs::create_dir_all(claude.parent().unwrap()).expect("mk .claude");
    std::fs::create_dir_all(codex.parent().unwrap()).expect("mk .codex");

    // Criterion 8: --no-hooks touches neither file.
    let out = upgrade(
        work.path(),
        home.path(),
        &["--finish", "--no-hooks", "--json"],
    );
    assert_eq!(json_hooks(&out), pair("skipped", "skipped"));
    assert!(!claude.exists() && !codex.exists());

    // Criterion 8: --dry-run (from the source repo, as upgrade requires)
    // reports the would-add rows and writes nothing.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = upgrade(repo, home.path(), &["--dry-run", "--json", "--no-gc"]);
    assert_eq!(json_hooks(&out), pair("would_add", "would_add"));
    let out = upgrade(
        repo,
        home.path(),
        &["--dry-run", "--json", "--no-gc", "--no-hooks"],
    );
    assert_eq!(json_hooks(&out), pair("skipped", "skipped"));
    assert!(!claude.exists() && !codex.exists());

    // Criteria 4 + 9: the real finish phase adds both and the human report
    // names each file and the Codex /hooks approval step.
    let out = upgrade(work.path(), home.path(), &["--finish"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hooks: claude  ~/.claude/settings.json: added SessionStart entry"),
        "{text}"
    );
    assert!(
        text.contains("hooks: codex   ~/.codex/hooks.json: added SessionStart entry")
            && text.contains("approve it once in Codex's /hooks screen"),
        "{text}"
    );
    assert_eq!(memhub_groups(&claude).len(), 1);
    assert_eq!(memhub_groups(&codex).len(), 1);

    // Criterion 5: a second upgrade adds no second entry.
    let out = upgrade(work.path(), home.path(), &["--finish", "--json"]);
    assert_eq!(json_hooks(&out), pair("already_present", "already_present"));
    assert_eq!(memhub_groups(&claude).len(), 1);
    assert_eq!(memhub_groups(&codex).len(), 1);

    // #298 criterion 2: a Codex entry from before the output limit is
    // reported by --dry-run without being written, then updated.
    let old = json!({"hooks": {"SessionStart": [{"matcher": "startup|resume",
        "hooks": [{"type": "command", "command": CMD, "timeout": 30}]}]}});
    std::fs::write(&codex, serde_json::to_vec_pretty(&old).unwrap()).expect("seed old codex");
    let old_bytes = std::fs::read(&codex).unwrap();
    let out = upgrade(repo, home.path(), &["--dry-run", "--json", "--no-gc"]);
    assert_eq!(json_hooks(&out), pair("already_present", "would_update"));
    assert_eq!(std::fs::read(&codex).unwrap(), old_bytes);
    let out = upgrade(work.path(), home.path(), &["--finish"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hooks: codex   ~/.codex/hooks.json: updated SessionStart entry")
            && text.contains("approve the changed hook again in Codex's /hooks screen"),
        "{text}"
    );
    assert_eq!(
        memhub_groups(&codex)[0]["hooks"][0]["additionalContextLimit"],
        4000
    );
}

#[test]
fn install_skips_missing_agent_dir_and_refuses_unwritable_file() {
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let codex_dir = home.path().join(".codex");
    let claude = home.path().join(".claude").join("settings.json");
    std::fs::create_dir_all(claude.parent().unwrap()).expect("mk .claude");

    // No ~/.codex: nothing is created for Codex, in a dry run or a real one,
    // so `memhub doctor` keeps reading Codex as not set up.
    for dry in [true, false] {
        let row = status_of(&install_session_hooks(dry), "codex");
        assert_eq!(row.status, HookStatus::Skipped, "dry={dry}");
        assert_eq!(row.detail.as_deref(), Some("no ~/.codex"));
        assert!(
            row.line().contains("skipped (no ~/.codex)"),
            "{}",
            row.line()
        );
        assert!(
            !codex_dir.exists(),
            "~/.codex must not be created (dry={dry})"
        );
    }
    assert_eq!(memhub_groups(&claude).len(), 1, "claude still installed");

    // A target without write permission is left unchanged, not replaced.
    // Forget the earlier install so memhub would otherwise add the entry.
    std::fs::remove_file(home.path().join(".memhub").join("installed-hooks.json"))
        .expect("rm record");
    std::fs::remove_file(&claude).expect("rm settings");
    std::fs::write(&claude, b"{}").expect("seed settings");
    let mut perms = std::fs::metadata(&claude).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&claude, perms.clone()).expect("make read-only");
    // Privileged users (e.g. root) can write anyway; nothing to assert then,
    // but say so. Written to the raw stderr handle, which libtest does not
    // capture, so the notice shows in a plain `cargo test` run.
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .open(&claude)
        .is_ok();
    if writable {
        let _ = writeln!(
            std::io::stderr(),
            "install_skips_missing_agent_dir_and_refuses_unwritable_file: \
             could not make the file read-only; read-only assertions SKIPPED"
        );
    } else {
        let row = status_of(&install_session_hooks(false), "claude");
        assert_eq!(row.status, HookStatus::NotInstalled, "{}", row.line());
        assert_eq!(std::fs::read(&claude).unwrap(), b"{}");
        let leftovers: Vec<_> = std::fs::read_dir(claude.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != "settings.json")
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&claude, perms).expect("restore");
}

#[test]
fn codex_entry_gains_context_limit_once_and_keeps_everything_else() {
    // #298 criteria 2 + 3.
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let codex = home.path().join(".codex").join("hooks.json");
    std::fs::create_dir_all(codex.parent().unwrap()).expect("mk .codex");
    let seeded = json!({
        "other": {"keep": [1, 2.5, "x", null]},
        "hooks": {
            "SessionStart": [
                {"matcher": "startup", "hooks": [{"type": "command", "command": "other-tool start"}]},
                {"matcher": "custom", "hooks": [
                    {"type": "command", "command": CMD, "timeout": 7, "statusMessage": "mine"}
                ]}
            ],
            "Stop": [{"hooks": [{"type": "command", "command": "other-tool stop"}]}]
        }
    });
    std::fs::write(&codex, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed");
    let seeded_bytes = std::fs::read(&codex).unwrap();

    let dry = status_of(&install_session_hooks(true), "codex");
    assert_eq!(dry.status, HookStatus::WouldUpdate, "{}", dry.line());
    assert_eq!(
        std::fs::read(&codex).unwrap(),
        seeded_bytes,
        "dry run wrote"
    );

    let row = status_of(&install_session_hooks(false), "codex");
    assert_eq!(row.status, HookStatus::Updated, "{}", row.line());
    assert!(
        row.line()
            .contains("approve the changed hook again in Codex's /hooks screen"),
        "{}",
        row.line()
    );
    let mut expected = seeded.clone();
    expected["hooks"]["SessionStart"][1]["hooks"][0]["additionalContextLimit"] = json!(4000);
    let after: Value = serde_json::from_slice(&std::fs::read(&codex).unwrap()).unwrap();
    assert_eq!(after, expected, "only the limit field may be added");

    // Criterion 3: once the field is there (any value) the file is left
    // byte-for-byte alone.
    let mut custom = seeded.clone();
    custom["hooks"]["SessionStart"][1]["hooks"][0]["additionalContextLimit"] = json!(9000);
    for limit_file in [
        std::fs::read(&codex).unwrap(),
        serde_json::to_vec(&custom).unwrap(),
    ] {
        std::fs::write(&codex, &limit_file).expect("seed limit");
        let row = status_of(&install_session_hooks(false), "codex");
        assert_eq!(row.status, HookStatus::AlreadyPresent, "{}", row.line());
        assert_eq!(std::fs::read(&codex).unwrap(), limit_file);
    }
}

#[test]
fn memhub_run_by_full_or_quoted_path_counts_as_present() {
    // #298 criterion 4.
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let claude = home.path().join(".claude").join("settings.json");
    let codex = home.path().join(".codex").join("hooks.json");
    std::fs::create_dir_all(claude.parent().unwrap()).expect("mk .claude");
    std::fs::create_dir_all(codex.parent().unwrap()).expect("mk .codex");
    let file_with = |command: &str| {
        json!({"hooks": {"SessionStart": [{"hooks": [
            {"type": "command", "command": command, "additionalContextLimit": 4000}
        ]}]}})
    };
    let quoted = r#""C:\Program Files\memhub\MEMHUB.exe" hook session-start"#;
    std::fs::write(&claude, serde_json::to_vec(&file_with(quoted)).unwrap()).expect("claude");
    std::fs::write(
        &codex,
        serde_json::to_vec(&file_with("/home/u/.cargo/bin/memhub hook session-start")).unwrap(),
    )
    .expect("codex");
    let (claude_bytes, codex_bytes) = (
        std::fs::read(&claude).unwrap(),
        std::fs::read(&codex).unwrap(),
    );
    let rows = install_session_hooks(false);
    assert!(
        rows.iter().all(|r| r.status == HookStatus::AlreadyPresent),
        "{rows:?}"
    );
    assert_eq!(std::fs::read(&claude).unwrap(), claude_bytes);
    assert_eq!(std::fs::read(&codex).unwrap(), codex_bytes);

    // Another program with the same arguments is not memhub's entry: the
    // real entry is added and the user's handler is left exactly as it was
    // (no limit field added). Forget the record first each time, or the
    // file would read as user-removed.
    for other in [
        "/usr/bin/other hook session-start",
        "other /usr/bin/memhub hook session-start",
    ] {
        std::fs::remove_file(home.path().join(".memhub").join("installed-hooks.json"))
            .expect("rm record");
        let users = json!({"hooks": {"SessionStart": [{"hooks": [
            {"type": "command", "command": other, "timeout": 5}
        ]}]}});
        std::fs::write(&codex, serde_json::to_vec(&users).unwrap()).expect("codex other");
        let row = status_of(&install_session_hooks(false), "codex");
        assert_eq!(row.status, HookStatus::Added, "{other:?}: {}", row.line());
        let after: Value = serde_json::from_slice(&std::fs::read(&codex).unwrap()).unwrap();
        assert_eq!(
            after["hooks"]["SessionStart"][0], users["hooks"]["SessionStart"][0],
            "{other:?}: the user's handler must not change"
        );
        assert_eq!(memhub_groups(&codex).len(), 1, "{other:?}");
    }
}

#[test]
fn removal_record_survives_home_spelling_and_old_record_format() {
    // #298 criterion 5.
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let claude = home.path().join(".claude").join("settings.json");
    let marker = home.path().join(".memhub").join("installed-hooks.json");
    std::fs::create_dir_all(claude.parent().unwrap()).expect("mk .claude");
    assert_eq!(
        status_of(&install_session_hooks(false), "claude").status,
        HookStatus::Added
    );
    std::fs::write(&claude, b"{}").expect("user removes the entry");

    // The same home with a `.` component.
    set_home(&home.path().join("."));
    let row = status_of(&install_session_hooks(false), "claude");
    assert_eq!(row.status, HookStatus::LeftOut, "{}", row.line());
    assert_eq!(std::fs::read(&claude).unwrap(), b"{}");

    // A record written by the code before #298: absolute path strings.
    let old_key = home
        .path()
        .join(".claude")
        .join("settings.json")
        .to_string_lossy()
        .into_owned();
    std::fs::write(&marker, serde_json::to_vec_pretty(&[old_key]).unwrap()).expect("old record");
    for spelling in [home.path().to_path_buf(), home.path().join(".")] {
        set_home(&spelling);
        let row = status_of(&install_session_hooks(false), "claude");
        assert_eq!(
            row.status,
            HookStatus::LeftOut,
            "{spelling:?}: {}",
            row.line()
        );
        assert_eq!(std::fs::read(&claude).unwrap(), b"{}");
    }
}

#[test]
fn dangling_symlink_hook_files_are_left_alone() {
    // #298 criterion 6.
    let _env_guard = crate::support::env_lock();
    let home = tempdir().expect("home");
    set_home(home.path());
    let mut links = Vec::new();
    for (dir, file) in [(".claude", "settings.json"), (".codex", "hooks.json")] {
        let dir = home.path().join(dir);
        std::fs::create_dir_all(&dir).expect("mk agent dir");
        let (link, target) = (dir.join(file), dir.join("missing-target.json"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &link);
        // Needs Developer Mode or admin on Windows.
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &link);
        if let Err(e) = made {
            let _ = writeln!(
                std::io::stderr(),
                "dangling_symlink_hook_files_are_left_alone: cannot create a symlink ({e}); SKIPPED"
            );
            return;
        }
        links.push((link, target));
    }
    for dry in [true, false] {
        let rows = install_session_hooks(dry);
        assert!(
            rows.iter().all(|r| r.status == HookStatus::NotInstalled),
            "dry={dry}: {rows:?}"
        );
        for (link, target) in &links {
            assert!(
                link.symlink_metadata().unwrap().file_type().is_symlink(),
                "{link:?} replaced"
            );
            assert_eq!(&std::fs::read_link(link).unwrap(), target);
            assert!(!target.exists(), "{target:?} created");
            assert_eq!(
                std::fs::read_dir(link.parent().unwrap()).unwrap().count(),
                1,
                "a file was created next to {link:?}"
            );
        }
    }
}

#[test]
fn agent_paths_missing_or_not_directories_are_skipped() {
    // #298 criteria 8 + 11.
    let _env_guard = crate::support::env_lock();

    // Neither ~/.claude nor ~/.codex: both skipped, nothing created.
    let home = tempdir().expect("home");
    set_home(home.path());
    for dry in [true, false] {
        let rows = install_session_hooks(dry);
        assert_eq!(
            status_of(&rows, "claude").detail.as_deref(),
            Some("no ~/.claude")
        );
        assert_eq!(
            status_of(&rows, "codex").detail.as_deref(),
            Some("no ~/.codex")
        );
        assert!(
            rows.iter().all(|r| r.status == HookStatus::Skipped),
            "{rows:?}"
        );
        assert!(!home.path().join(".claude").exists() && !home.path().join(".codex").exists());
    }

    // Only ~/.codex: Claude Code skipped, Codex installed.
    std::fs::create_dir_all(home.path().join(".codex")).expect("mk .codex");
    let rows = install_session_hooks(false);
    assert_eq!(status_of(&rows, "claude").status, HookStatus::Skipped);
    assert_eq!(status_of(&rows, "codex").status, HookStatus::Added);
    assert!(!home.path().join(".claude").exists());
    assert_eq!(
        memhub_groups(&home.path().join(".codex").join("hooks.json")).len(),
        1
    );

    // Both paths exist as regular files: reported, nothing touched.
    let home = tempdir().expect("home");
    set_home(home.path());
    std::fs::write(home.path().join(".claude"), b"not a dir").expect("file .claude");
    std::fs::write(home.path().join(".codex"), b"not a dir").expect("file .codex");
    let before = tree(home.path());
    for dry in [true, false] {
        let rows = install_session_hooks(dry);
        assert!(
            rows.iter().all(|r| r.status == HookStatus::Skipped),
            "{rows:?}"
        );
        assert_eq!(
            status_of(&rows, "claude").detail.as_deref(),
            Some("~/.claude is not a directory")
        );
        assert_eq!(
            status_of(&rows, "codex").detail.as_deref(),
            Some("~/.codex is not a directory")
        );
        assert_eq!(tree(home.path()), before, "dry={dry}: nothing may change");
    }
}

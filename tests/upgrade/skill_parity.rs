//! Contract test: the Claude, Codex, and OpenCode agent surfaces must stay in
//! parity, and the README install blocks plus the two tracked
//! orientation files must not drift away from the actual skill set.
//!
//! Metrics templates intentionally remain in parity as dormant
//! reactivation assets, but default install blocks exclude them while the
//! subsystem is hibernated.
//!
//! There is no CI in this repo — `cargo test` is the gate. Adding a
//! new skill, or a new `## ` section to one orientation file, now
//! forces the matching update everywhere or this test fails.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Skills that are intentionally only on one side. Empty today — all
/// agents expose the identical set. A future intentional divergence
/// goes here *with a comment*, so "we meant that" is explicit and the
/// reviewer sees it in the diff rather than the test silently passing.
const CLAUDE_ONLY_SKILLS: &[&str] = &[];
const CODEX_ONLY_SKILLS: &[&str] = &[];
const OPENCODE_ONLY_SKILLS: &[&str] = &[];

/// N4 keystone phrases that must survive the CLAUDE.md token diet (issue
/// #30). Each is a safety gate, an identity line, or a guardrail an agent
/// must see inline — the diet may relocate prose into
/// `docs/reference/operations.md`, but never these.
///
/// Canonical definition lives in `commands::audit_md` (issue #32): its
/// `memhub audit md` keystone check asserts the exact same set, so the
/// list is imported rather than duplicated here — otherwise the audit
/// and this contract test could silently drift apart from each other.
use memhub::commands::audit_md::CLAUDE_KEYSTONE_PHRASES;

/// Claude skills are flat `templates/skills/claude/<name>.md`.
fn claude_skill_names() -> BTreeSet<String> {
    let dir = repo_root().join("templates/skills/claude");
    fs::read_dir(&dir)
        .expect("read templates/skills/claude")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("md") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Codex skills are `templates/skills/codex/<name>/SKILL.md`.
fn codex_skill_names() -> BTreeSet<String> {
    let dir = repo_root().join("templates/skills/codex");
    fs::read_dir(&dir)
        .expect("read templates/skills/codex")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| {
            e.path()
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// OpenCode skills are active templates under `opencode/` plus dormant
/// reactivation templates kept outside that configured discovery root.
fn opencode_skill_names() -> BTreeSet<String> {
    let mut names = dir_per_skill_names("templates/skills/opencode");
    names.extend(dir_per_skill_names("templates/skills/opencode-hibernated"));
    names
}

fn dir_per_skill_names(relative: &str) -> BTreeSet<String> {
    let dir = repo_root().join(relative);
    fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("read {relative}"))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| {
            e.path()
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect()
}

fn opencode_command_names() -> BTreeSet<String> {
    let dir = repo_root().join("templates/commands/opencode");
    fs::read_dir(&dir)
        .expect("read templates/commands/opencode")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("md") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// The canonical skill set both agents must expose: every skill that
/// is not on an allowlist must appear on both sides.
fn canonical_skill_set() -> BTreeSet<String> {
    let mut set = claude_skill_names();
    set.extend(codex_skill_names());
    set.extend(opencode_skill_names());
    for s in CLAUDE_ONLY_SKILLS
        .iter()
        .chain(CODEX_ONLY_SKILLS.iter())
        .chain(OPENCODE_ONLY_SKILLS.iter())
    {
        set.remove(*s);
    }
    set
}

fn default_installed_skill_set() -> BTreeSet<String> {
    let mut set = canonical_skill_set();
    set.remove("metrics");
    set
}

#[test]
fn agent_skill_template_sets_match() {
    let claude = claude_skill_names();
    let codex = codex_skill_names();
    let opencode = opencode_skill_names();

    assert!(!claude.is_empty(), "no Claude skill templates discovered");
    assert!(!codex.is_empty(), "no Codex skill templates discovered");
    assert!(
        !opencode.is_empty(),
        "no OpenCode skill templates discovered"
    );

    let allowed_claude_only: BTreeSet<String> =
        CLAUDE_ONLY_SKILLS.iter().map(|s| s.to_string()).collect();
    let allowed_codex_only: BTreeSet<String> =
        CODEX_ONLY_SKILLS.iter().map(|s| s.to_string()).collect();
    let allowed_opencode_only: BTreeSet<String> =
        OPENCODE_ONLY_SKILLS.iter().map(|s| s.to_string()).collect();

    let canonical = canonical_skill_set();
    let claude_only: BTreeSet<_> = claude.difference(&canonical).cloned().collect();
    let codex_only: BTreeSet<_> = codex.difference(&canonical).cloned().collect();
    let opencode_only: BTreeSet<_> = opencode.difference(&canonical).cloned().collect();
    let missing_claude: BTreeSet<_> = canonical.difference(&claude).cloned().collect();
    let missing_codex: BTreeSet<_> = canonical.difference(&codex).cloned().collect();
    let missing_opencode: BTreeSet<_> = canonical.difference(&opencode).cloned().collect();

    assert_eq!(
        claude_only, allowed_claude_only,
        "unexpected Claude-only skills"
    );
    assert_eq!(
        codex_only, allowed_codex_only,
        "unexpected Codex-only skills"
    );
    assert_eq!(
        opencode_only, allowed_opencode_only,
        "unexpected OpenCode-only skills"
    );
    assert!(
        missing_claude.is_empty(),
        "missing Claude skills: {missing_claude:?}"
    );
    assert!(
        missing_codex.is_empty(),
        "missing Codex skills: {missing_codex:?}"
    );
    assert!(
        missing_opencode.is_empty(),
        "missing OpenCode skills: {missing_opencode:?}"
    );
}

#[test]
fn opencode_command_wrappers_match_skill_set() {
    let commands = opencode_command_names();
    let canonical = canonical_skill_set();

    assert_eq!(
        commands, canonical,
        "OpenCode command wrappers must match the canonical memhub skill set"
    );
}

/// Parse the tracked repo-root `opencode.json`'s native V2 `commands` block and
/// return its key set. This is the file OpenCode actually loads for an
/// in-repo session — distinct from `templates/commands/opencode/*.md`,
/// which only feeds `memhub upgrade`'s user-level install. The two can
/// drift independently (task 124 sweep / Q43): `opencode.json` silently
/// missed `catch-up` and `locate` while carrying the two hibernated
/// entries, and nothing parsed it to notice.
fn opencode_json_command_names() -> BTreeSet<String> {
    let path = repo_root().join("opencode.json");
    let raw = fs::read_to_string(&path).expect("read opencode.json");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse opencode.json");
    let commands = value
        .get("commands")
        .and_then(|v| v.as_object())
        .expect("opencode.json must have an object `commands` block");
    commands.keys().cloned().collect()
}

fn opencode_json_skill_sources() -> BTreeSet<String> {
    let path = repo_root().join("opencode.json");
    let raw = fs::read_to_string(&path).expect("read opencode.json");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse opencode.json");
    value
        .get("skills")
        .and_then(|v| v.as_array())
        .expect("opencode.json must have a `skills` array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("opencode.json skill sources must be strings")
                .to_string()
        })
        .collect()
}

fn collect_opencode_skill_ids(source: &Path, dir: &Path, ids: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|_| panic!("read {}", dir.display())) {
        let path = entry.expect("read skill source entry").path();
        if path.is_dir() {
            collect_opencode_skill_ids(source, &path, ids);
        } else if dir == source && path.extension().and_then(|x| x.to_str()) == Some("md") {
            ids.insert(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .expect("root skill filename must be UTF-8")
                    .to_string(),
            );
        } else if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
            ids.insert(
                dir.file_name()
                    .and_then(|s| s.to_str())
                    .expect("skill directory name must be UTF-8")
                    .to_string(),
            );
        }
    }
}

fn opencode_json_discovered_skill_names() -> BTreeSet<String> {
    opencode_discovered_skill_names(&opencode_json_skill_sources())
}

fn opencode_discovered_skill_names(skill_sources: &BTreeSet<String>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for relative in skill_sources {
        let source = repo_root().join(relative);
        collect_opencode_skill_ids(&source, &source, &mut ids);
    }
    ids
}

#[test]
fn opencode_json_commands_block_matches_default_installed_skill_set() {
    let commands = opencode_json_command_names();
    let canonical = default_installed_skill_set();

    assert_eq!(
        commands, canonical,
        "opencode.json's `commands` block must define exactly the canonical \
         memhub skill set minus the hibernated metrics skill — it must \
         gain a skill the moment one ships and lose metrics, which stays \
         hibernated in a default build"
    );
}

#[test]
fn opencode_json_discovers_only_default_installed_skills() {
    assert_eq!(
        opencode_json_discovered_skill_names(),
        default_installed_skill_set(),
        "opencode.json must discover every active skill by its path-derived ID without \
         scanning the metrics reactivation templates"
    );
}

#[test]
#[ignore = "requires an installed OpenCode V2 parser; run explicitly"]
fn opencode_v2_effective_config_smoke() {
    let root = repo_root();
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/d", "/c", "opencode2", "debug", "config"]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("opencode2");
        command.args(["debug", "config"]);
        command
    };
    let output = command
        .current_dir(&root)
        .output()
        .expect("run installed OpenCode V2 parser (`opencode2 debug config`)");
    assert!(
        output.status.success(),
        "`opencode2 debug config` failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let entries: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("parse `opencode2 debug config` output: {error}"));
    let config_path = root
        .join("opencode.json")
        .canonicalize()
        .expect("canonicalize repo-root opencode.json");
    let project_document = entries
        .as_array()
        .expect("OpenCode debug config output must be an array")
        .iter()
        .find(|entry| {
            entry.get("type").and_then(|value| value.as_str()) == Some("document")
                && entry
                    .get("path")
                    .and_then(|value| value.as_str())
                    .and_then(|path| Path::new(path).canonicalize().ok())
                    .is_some_and(|path| path == config_path)
        })
        .expect("OpenCode debug config must include the repo-root opencode.json document");
    let info = project_document
        .get("info")
        .expect("repo-root opencode.json document must include parsed info");

    assert!(
        info.pointer("/mcp/servers/memhub").is_some(),
        "OpenCode must parse the native `mcp.servers.memhub` project configuration"
    );
    let commands: BTreeSet<String> = info
        .get("commands")
        .and_then(|value| value.as_object())
        .expect("OpenCode must parse the native project `commands` block")
        .keys()
        .cloned()
        .collect();
    assert_eq!(commands, default_installed_skill_set());

    let skill_sources: BTreeSet<String> = info
        .get("skills")
        .and_then(|value| value.as_array())
        .expect("OpenCode must parse the native project `skills` array")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("OpenCode project skill sources must be strings")
                .to_string()
        })
        .collect();
    assert_eq!(
        skill_sources,
        BTreeSet::from(["templates/skills/opencode".to_string()]),
        "OpenCode must expose only the active project skill source"
    );
    assert_eq!(
        opencode_discovered_skill_names(&skill_sources),
        default_installed_skill_set(),
        "OpenCode project discovery must exclude the hibernated metrics skill"
    );
}

/// Pull every `/skill-name` token out of a README enumeration segment.
fn slash_tokens(segment: &str) -> BTreeSet<String> {
    let bytes = segment.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len()
                && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'-')
            {
                j += 1;
            }
            if j > start {
                out.insert(segment[start..j].to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn readme_install_blocks_enumerate_every_skill() {
    let readme = fs::read_to_string(repo_root().join("README.md")).expect("read README.md");

    // The Claude, Codex, and OpenCode install blocks carry one stable
    // sentence: "Copy the user-level skills so <list> all work".
    // Scrape each occurrence's enumeration and require the full set.
    let marker = "Copy the user-level skills so";
    let mut segments = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = readme[search_from..].find(marker) {
        let start = search_from + rel;
        let rest = &readme[start..];
        let end = rest
            .find("all work")
            .expect("skill enumeration must end with 'all work'");
        segments.push(rest[..end].to_string());
        search_from = start + marker.len();
    }

    assert_eq!(
        segments.len(),
        3,
        "expected exactly three install-block skill enumerations \
         (Claude + Codex + OpenCode); found {}",
        segments.len()
    );

    let canonical = default_installed_skill_set();
    for (idx, seg) in segments.iter().enumerate() {
        let listed = slash_tokens(seg);
        assert_eq!(
            listed,
            canonical,
            "README skill enumeration #{} is out of sync with the skill \
             template set.\n  listed:    {:?}\n  canonical: {:?}\n\
             (update the 'Copy the user-level skills so ... all work' \
             sentence in both install blocks when you add or remove a skill)",
            idx + 1,
            listed,
            canonical
        );
    }
}

/// Creation tokens (raw, unnormalized destination paths) of a `mkdir -p` or
/// `New-Item -ItemType Directory ... -Path a,b` README line; empty otherwise.
/// A `mkdir -p` list ends at the first `;`, so a creation joined to a copy on
/// one line (`mkdir -p d; for ...; cp "$f" d/; done`) does not count the
/// copy's destination as a second creation.
fn created_tokens(line: &str) -> Vec<&str> {
    if let Some(i) = line.find("mkdir -p ") {
        let mut tokens = Vec::new();
        for t in line[i + "mkdir -p ".len()..].split_whitespace() {
            let path = t.trim_end_matches(';');
            if !path.is_empty() {
                tokens.push(path);
            }
            if path.len() != t.len() {
                break;
            }
        }
        tokens
    } else if line.contains("New-Item -ItemType Directory") {
        let Some((_, rest)) = line.split_once("-Path ") else {
            return Vec::new();
        };
        let paths = rest.split(" |").next().unwrap_or(rest);
        paths
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}

/// Canonical form so `~/.x/y/` (POSIX) and `"$HOME\.x\y\"` (PowerShell) of
/// the same directory compare equal.
fn norm_install_path(token: &str) -> String {
    let t = token
        .trim_matches(|c| c == '"' || c == ';')
        .replace('\\', "/");
    let t = match t.strip_prefix('~') {
        Some(rest) => format!("$HOME{rest}"),
        None => t,
    };
    t.trim_end_matches('/').to_string()
}

/// The single definition of a skill/command-wrapper copy line, shared by the
/// checker, the mutation battery, and the grown-README test: a `cp` or
/// `Copy-Item` line copying from `templates/skills` or `templates/commands`
/// (either slash direction). `Some(is_powershell)` for a copy line.
fn template_copy_kind(line: &str) -> Option<bool> {
    let from_wrappers = [
        "templates/skills/",
        "templates/commands/",
        "templates\\skills\\",
        "templates\\commands\\",
    ]
    .iter()
    .any(|p| line.contains(p));
    if !from_wrappers {
        None
    } else if line.contains("Copy-Item") {
        Some(true)
    } else {
        line.contains("cp ").then_some(false)
    }
}

/// Destination directory of a skill/command copy line, raw. The PowerShell
/// half quotes it right after `$_.FullName`; the POSIX half puts it after
/// the quoted source in `cp [-R] "$x" DEST`.
fn copy_destination(line: &str) -> Option<&str> {
    if let Some((_, rest)) = line.split_once("Copy-Item $_.FullName ") {
        rest.strip_prefix('"')?.split('"').next()
    } else {
        let (_, rest) = line.rsplit_once("cp ")?;
        rest.split_whitespace()
            .filter(|t| !t.starts_with('-'))
            .nth(1)
    }
}

/// The names a copy line excludes: the `case "$(basename ..)" in a|b)` pattern
/// of a POSIX line (which must also `continue`), or the `-notin 'a','b'` list
/// of a PowerShell line. `None` when the line has no exclusion at all.
fn copy_exclusions(line: &str, is_ps: bool) -> Option<Vec<String>> {
    let list = if is_ps {
        let (_, rest) = line.split_once("-notin ")?;
        rest.split(" }").next()?.split(',').collect::<Vec<_>>()
    } else {
        let (_, rest) = line.split_once("\" in ")?;
        let (pattern, after) = rest.split_once(')')?;
        if !after.contains("continue") {
            return None;
        }
        pattern.split('|').collect()
    };
    Some(
        list.into_iter()
            .map(|t| t.trim().trim_matches('\'').to_string())
            .collect(),
    )
}

/// The single definition of an install block, shared by the checker and the
/// mutation battery: the index of the `<details>` block each README line
/// belongs to (0 before the first `<details>`).
fn install_block_of_line(readme: &str) -> Vec<usize> {
    let mut block = 0;
    readme
        .lines()
        .map(|line| {
            block += line.matches("<details>").count();
            block
        })
        .collect()
}

/// Checks every skill/command-wrapper copy line of every README `<details>`
/// block, in both the POSIX (`cp`) and PowerShell (`Copy-Item`) halves:
/// it must skip exactly the hibernated `metrics` subsystem, in the form that
/// matches what it copies (`metrics.md` for a `*.md` file glob, `metrics`
/// for a directory copy), and its destination must have been created by an
/// earlier `mkdir -p` / `New-Item -ItemType Directory` line of the same
/// block (without that, `Copy-Item -Recurse` into a missing directory turns
/// the first skill directory into the destination — task 137). Returns the
/// number of `(posix, powershell)` copy lines checked, or the first
/// violation.
fn check_readme_install_blocks(readme: &str) -> Result<(usize, usize), String> {
    let (mut posix, mut ps) = (0, 0);
    let (mut current, mut created) = (0, BTreeSet::new());
    let mut copy_blocks = BTreeSet::new();
    for (raw, block) in readme.lines().zip(install_block_of_line(readme)) {
        if block != current {
            (current, created) = (block, BTreeSet::new());
        }
        let line = raw.trim();
        // Each half must create its own directories.
        created.extend(
            created_tokens(line)
                .into_iter()
                .map(|t| (line.contains("New-Item"), norm_install_path(t))),
        );
        let Some(is_ps) = template_copy_kind(line) else {
            continue;
        };
        copy_blocks.insert(block);
        let is_dir = if is_ps {
            line.contains("-Directory")
        } else {
            line.contains("cp -R")
        };
        let (form, expected) = if is_dir {
            ("directory", "metrics")
        } else {
            ("file", "metrics.md")
        };
        match copy_exclusions(line, is_ps) {
            None => {
                return Err(format!(
                    "README copies from a skill/command template directory \
                     without a metrics exclusion — the hibernated metrics \
                     subsystem must never be installed by a default build: \
                     {line:?}"
                ));
            }
            Some(found) if found != [expected] => {
                return Err(format!(
                    "README {form} copy line excludes {found:?} but must \
                     exclude exactly [{expected:?}] (the {form} form of the \
                     hibernated metrics template): {line:?}"
                ));
            }
            Some(_) => {}
        }
        let dest = copy_destination(line)
            .map(norm_install_path)
            .ok_or_else(|| format!("cannot find the copy destination in {line:?}"))?;
        if !created.contains(&(is_ps, dest.clone())) {
            return Err(format!(
                "README copy line writes into {dest:?}, which no earlier \
                 mkdir -p / New-Item -ItemType Directory line in the same \
                 install block creates: {line:?}"
            ));
        }
        if is_ps {
            ps += 1;
        } else {
            posix += 1;
        }
    }
    // Claude, Codex, OpenCode quickstarts + Install by hand; more copy
    // lines per block are fine, fewer blocks are not.
    let blocks = copy_blocks.len();
    if blocks < 4 || posix < 8 || ps < 8 {
        return Err(format!(
            "expected skill-copy lines in at least 4 install blocks (Claude/\
             Codex/OpenCode quickstarts + Install by hand), 8 POSIX and 8 \
             PowerShell lines in total; found {blocks} blocks, {posix} \
             POSIX, {ps} PowerShell"
        ));
    }
    Ok((posix, ps))
}

/// Onboarding surfaces must never install or offer the hibernated metrics
/// subsystem in a default build. The three CLI quickstart blocks already
/// skip it with a `case ... continue;;` guard (POSIX) or a
/// `Where-Object { $_.Name -notin ... }` filter (PowerShell) on the command
/// that copies each skill; "Install by hand" used to copy the same globs
/// with a bare `cp`, silently installing `metrics.md` and the
/// `codex`/`opencode` `metrics/` directories. This scans every
/// skill/command-wrapper copy line in the whole README — including Install
/// by hand — and fails if any of them lost the guard or writes into a
/// directory its block never creates.
#[test]
fn readme_skill_copy_commands_skip_metrics() {
    let readme = fs::read_to_string(repo_root().join("README.md")).expect("read README.md");
    if let Err(msg) = check_readme_install_blocks(&readme) {
        panic!("{msg}");
    }
}

/// Feeds `check_readme_install_blocks` mutated copies of `readme` (one line
/// changed at a time) and requires each to be rejected for the expected
/// reason: every copy line losing its metrics guard or swapping it for the
/// wrong form, and every destination-creation line (or each of its paths)
/// being removed. Reasons are asserted, not just failure.
fn assert_checker_rejects_mutations(readme: &str) {
    check_readme_install_blocks(readme).expect("unmutated README passes");

    // Replace the line at `idx` with `new_line` and require a rejection whose
    // message contains `reason`.
    let reject = |idx: usize, new_line: &str, reason: &str, what: &str| {
        let mutated: Vec<&str> = readme
            .lines()
            .enumerate()
            .map(|(i, l)| if i == idx { new_line } else { l })
            .collect();
        let err = check_readme_install_blocks(&mutated.join("\n")).expect_err(&format!(
            "checker accepted a README with {what}: line {} was {:?}",
            idx + 1,
            readme.lines().nth(idx).unwrap()
        ));
        assert!(
            err.contains(reason),
            "checker rejected {what} (line {}) for the wrong reason, wanted {reason:?}: {err}",
            idx + 1
        );
    };
    // Remove `line[from..to]`, where `to` is the start of `until` after `from`.
    let cut = |line: &str, from: &str, until: &str| -> String {
        let a = line.find(from).expect("guard start present");
        let b = a + line[a..].find(until).expect("guard end present");
        format!("{}{}", &line[..a], &line[b..])
    };
    // Swap the exclusion for the other form: `metrics.md` <-> `metrics`.
    let swap_form = |line: &str, is_ps: bool, is_dir: bool| -> String {
        let (from, to) = match (is_ps, is_dir) {
            (false, false) => ("in metrics.md)", "in metrics)"),
            (false, true) => ("in metrics)", "in metrics.md)"),
            (true, false) => ("-notin 'metrics.md'", "-notin 'metrics'"),
            (true, true) => ("-notin 'metrics'", "-notin 'metrics.md'"),
        };
        assert!(line.contains(from), "expected {from:?} in {line:?}");
        line.replacen(from, to, 1)
    };

    // Only creations of a directory some copy line writes into matter (the
    // README's `mkdir -p ~/gdrive` for Drive sync is unrelated).
    let destinations: BTreeSet<String> = readme
        .lines()
        .map(str::trim)
        .filter(|l| template_copy_kind(l).is_some())
        .filter_map(copy_destination)
        .map(norm_install_path)
        .collect();

    // Which lines of each install block create each (half, destination), and
    // which copy lines write into it. Deleting a creation line is only
    // accepted when another creation still precedes every such copy line.
    let block_of_line = install_block_of_line(readme);
    type Key = (usize, bool, String);
    let mut creators: std::collections::BTreeMap<Key, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut copies: std::collections::BTreeMap<Key, Vec<usize>> = std::collections::BTreeMap::new();
    for (idx, line) in readme.lines().enumerate() {
        let is_ps = line.contains("New-Item");
        for token in created_tokens(line) {
            creators
                .entry((block_of_line[idx], is_ps, norm_install_path(token)))
                .or_default()
                .push(idx);
        }
        if let Some((is_ps, dest)) = template_copy_kind(line).zip(copy_destination(line.trim())) {
            copies
                .entry((block_of_line[idx], is_ps, norm_install_path(dest)))
                .or_default()
                .push(idx);
        }
    }
    // True when dropping `token` from the creation on line `idx` leaves a copy
    // line writing into it with no creation left before it in its block (a
    // creation on the copy's own line counts as before it, as in the checker).
    // `line_deleted`: the whole line goes, so a copy on it goes too.
    let needs_rejection = |idx: usize, token: &str, line_deleted: bool| {
        let key = (
            block_of_line[idx],
            readme.lines().nth(idx).unwrap().contains("New-Item"),
            norm_install_path(token),
        );
        copies.get(&key).is_some_and(|copy_lines| {
            copy_lines.iter().any(|&c| {
                (!line_deleted || c != idx) && !creators[&key].iter().any(|&j| j != idx && j <= c)
            })
        })
    };

    let (mut guards, mut creations) = (0, 0);
    for (idx, line) in readme.lines().enumerate() {
        if let Some(is_ps) = template_copy_kind(line) {
            guards += 1;
            let (unguarded, what) = if is_ps {
                (
                    cut(line, "| Where-Object", "| ForEach-Object"),
                    "a PowerShell copy line lacking the metrics exclusion",
                )
            } else {
                (
                    cut(line, "case ", "cp "),
                    "a POSIX copy line lacking the metrics exclusion",
                )
            };
            reject(idx, &unguarded, "without a metrics exclusion", what);
            let is_dir = if is_ps {
                line.contains("-Directory")
            } else {
                line.contains("cp -R")
            };
            reject(
                idx,
                &swap_form(line, is_ps, is_dir),
                "copy line excludes",
                "a copy line excluding the wrong form of the metrics template",
            );
        }
        let tokens: Vec<&str> = created_tokens(line)
            .into_iter()
            .filter(|t| destinations.contains(&norm_install_path(t)))
            .collect();
        if !tokens.is_empty() {
            creations += 1;
            if tokens.iter().any(|t| needs_rejection(idx, t, true)) {
                reject(idx, "", "no earlier", "a destination-creation line deleted");
            }
            for token in tokens {
                if needs_rejection(idx, token, false) {
                    reject(
                        idx,
                        &line.replacen(token, "", 1),
                        "no earlier",
                        "a destination path dropped from its creation line",
                    );
                }
            }
        }
    }
    // The README ships 8 POSIX + 8 PowerShell copy lines and 10 creation
    // lines (1 per half in each quickstart, 2 per half in Install by hand);
    // legitimate growth only raises these, so they are floors.
    assert!(guards >= 16, "mutated only {guards} copy lines");
    assert!(creations >= 10, "mutated only {creations} creation lines");
}

/// Proves the checker actually rejects each defect, on the real README.
#[test]
fn readme_install_block_checker_rejects_mutations() {
    let readme = lf(&fs::read_to_string(repo_root().join("README.md")).expect("read README.md"));
    assert_checker_rejects_mutations(&readme);
}

/// Legitimate README growth must not trip either install-block test: one
/// more correctly guarded copy line (its destination already created) after
/// every existing copy line, plus a repeat of each block's creation lines
/// placed after all of its copies (which must not count as creating a
/// destination in time), still passes the checker and the mutation battery.
/// No copy line gains a second creation ahead of it, so the battery still
/// demands a rejection for deleting every original creation line in every
/// half of every block.
#[test]
fn readme_install_block_checker_tolerates_added_copy_lines() {
    let readme = lf(&fs::read_to_string(repo_root().join("README.md")).expect("read README.md"));
    let mut grown = Vec::new();
    let mut repeated_after_copy = false;
    let mut creations: Vec<&str> = Vec::new();
    for line in readme.lines() {
        if line.contains("</details>") {
            repeated_after_copy |= !creations.is_empty();
            grown.append(&mut creations);
        }
        grown.push(line);
        if template_copy_kind(line).is_some() {
            grown.push(line);
        } else if !created_tokens(line).is_empty() {
            creations.push(line);
        }
    }
    assert!(
        repeated_after_copy,
        "no creation line to repeat after a copy"
    );
    let grown = grown.join("\n");
    check_readme_install_blocks(&grown).expect("README with extra guarded copy lines passes");
    assert_checker_rejects_mutations(&grown);
}

/// An extra destination-creation line ahead of a copy (here: every creation
/// line repeated right after itself) is harmless README growth too: it still
/// passes the checker and the mutation battery, which then no longer demands
/// a rejection for deleting one of the two identical creations.
#[test]
fn readme_install_block_checker_tolerates_extra_creation_before_copy() {
    let readme = lf(&fs::read_to_string(repo_root().join("README.md")).expect("read README.md"));
    let mut grown = Vec::new();
    for line in readme.lines() {
        grown.push(line);
        if !created_tokens(line).is_empty() {
            grown.push(line);
        }
    }
    let grown = grown.join("\n");
    check_readme_install_blocks(&grown).expect("README with repeated creation lines passes");
    assert_checker_rejects_mutations(&grown);
}

/// One line (PowerShell and POSIX alike) that both creates a destination
/// directory and copies into it (the creation counts as preceding the copy on
/// the same line) must pass the checker, and deleting it must not demand a
/// rejection: the copy goes with the line.
#[test]
fn readme_install_block_checker_accepts_combined_create_and_copy_line() {
    let readme = lf(&fs::read_to_string(repo_root().join("README.md")).expect("read README.md"));
    let lines: Vec<&str> = readme.lines().collect();
    for is_ps in [true, false] {
        let i = (0..lines.len() - 1)
            .find(|&i| {
                let tokens = created_tokens(lines[i]);
                lines[i].contains("New-Item") == is_ps
                    && template_copy_kind(lines[i + 1]) == Some(is_ps)
                    && tokens.len() == 1
                    && copy_destination(lines[i + 1].trim()).map(norm_install_path)
                        == Some(norm_install_path(tokens[0]))
            })
            .unwrap_or_else(|| panic!("no creation line (powershell: {is_ps}) followed by a copy"));
        let combined = format!("{}; {}", lines[i].trim_end(), lines[i + 1].trim());
        let variant = [&lines[..i], &[combined.as_str()], &lines[i + 2..]]
            .concat()
            .join("\n");
        check_readme_install_blocks(&variant)
            .unwrap_or_else(|e| panic!("combined line (powershell: {is_ps}) rejected: {e}"));
        assert_checker_rejects_mutations(&variant);
    }
}

/// Normalize line endings for a cross-platform byte comparison: this repo is
/// `core.autocrlf=true`, so a Windows checkout has CRLF on disk while the
/// generator emits LF.
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// `AGENTS.md` is a pure derivative of `CLAUDE.md`, not a hand-maintained
/// twin (issue #30 / decision Q21). This asserts content-equality against
/// the generator, replacing the older header-only parity check — so the two
/// files can no longer silently drift in prose, only in structure.
///
/// Regeneration path: `MEMHUB_REGEN=1 cargo test skill_parity` rewrites
/// `AGENTS.md` from `CLAUDE.md` and passes; commit the result. A normal run is
/// read-only and fails if the committed `AGENTS.md` is stale. (Wave 5 U4,
/// issue #90: this test now lives in the shared `upgrade_harness` binary —
/// there is no more per-file `--test skill_parity` target, but the
/// substring filter above still selects exactly this test.)
#[test]
fn agents_md_is_generated_from_claude_md() {
    let claude = fs::read_to_string(repo_root().join("CLAUDE.md")).expect("read CLAUDE.md");
    let generated = memhub::agents_md::generate_agents_md(&claude);
    let agents_path = repo_root().join("AGENTS.md");

    if std::env::var_os("MEMHUB_REGEN").is_some() {
        fs::write(&agents_path, &generated).expect("write AGENTS.md");
    }

    let agents = fs::read_to_string(&agents_path).expect("read AGENTS.md");
    assert_eq!(
        lf(&agents),
        lf(&generated),
        "AGENTS.md is out of sync with CLAUDE.md. Regenerate it with \
         `MEMHUB_REGEN=1 cargo test skill_parity` and commit AGENTS.md."
    );
}

/// N4: the CLAUDE.md token diet must relocate prose into
/// `docs/reference/operations.md` without dropping the load-bearing phrases
/// an agent needs inline (the two safety gates, the identity line, the core
/// guardrail).
#[test]
fn claude_md_keeps_keystone_phrases() {
    let claude = fs::read_to_string(repo_root().join("CLAUDE.md")).expect("read CLAUDE.md");
    let missing: Vec<&str> = CLAUDE_KEYSTONE_PHRASES
        .iter()
        .filter(|phrase| !claude.contains(**phrase))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "CLAUDE.md lost keystone phrase(s) during the token diet: {missing:?}"
    );
}

/// C4 (issue #31 / decision Q23): `CLAUDE.md` must carry a versioned,
/// machine-parseable `memhub:managed-block`, and it must survive the
/// `generate_agents_md` transform into `AGENTS.md` unchanged (the block
/// rides through with the rest of the body — it is not on the
/// injected/allowlisted-divergence list). This is the "governs the real
/// shipped file" counterpart to `managed_block::tests::parses_a_well_formed_block`,
/// which only exercises the parser against a synthetic fixture.
#[test]
fn claude_md_managed_block_parses() {
    let claude = fs::read_to_string(repo_root().join("CLAUDE.md")).expect("read CLAUDE.md");
    let block = memhub::managed_block::parse_managed_block(&claude)
        .expect("CLAUDE.md must carry a parseable memhub:managed-block");

    assert_eq!(block.version, memhub::managed_block::MANAGED_BLOCK_VERSION);
    assert_eq!(block.field("memhub-primary"), Some("true"));
    assert_eq!(block.field("db"), Some(".memhub/project.sqlite"));
    assert_eq!(block.field("rendered"), Some(".memhub/rendered/"));
    assert_eq!(block.field("config"), Some(".memhub/config.toml"));

    let agents = fs::read_to_string(repo_root().join("AGENTS.md")).expect("read AGENTS.md");
    let agents_block = memhub::managed_block::parse_managed_block(&agents)
        .expect("AGENTS.md must carry the same managed block, propagated by the generator");
    assert_eq!(
        block, agents_block,
        "managed block must propagate from CLAUDE.md into AGENTS.md unchanged"
    );
}

/// Every skill template file across all three agents, as absolute paths:
/// `templates/skills/claude/*.md`, `templates/skills/codex/*/SKILL.md`,
/// active and hibernated `templates/skills/opencode*/*/SKILL.md` roots.
fn all_skill_template_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    let claude_dir = repo_root().join("templates/skills/claude");
    for entry in fs::read_dir(&claude_dir).expect("read templates/skills/claude") {
        let path = entry.expect("read dir entry").path();
        if path.extension().and_then(|x| x.to_str()) == Some("md") {
            files.push(path);
        }
    }

    for relative in [
        "templates/skills/codex",
        "templates/skills/opencode",
        "templates/skills/opencode-hibernated",
    ] {
        let dir = repo_root().join(relative);
        for entry in fs::read_dir(&dir).unwrap_or_else(|_| panic!("read {relative}")) {
            let path = entry.expect("read dir entry").path();
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                files.push(skill_md);
            }
        }
    }

    files
}

#[test]
fn wrap_up_templates_render_with_actor_and_resume_without_replaying_writes() {
    for (relative, actor) in [
        ("templates/skills/claude/wrap-up.md", "claude:wrap-up"),
        ("templates/skills/codex/wrap-up/SKILL.md", "codex:wrap-up"),
        (
            "templates/skills/opencode/wrap-up/SKILL.md",
            "opencode:wrap-up",
        ),
    ] {
        let path = repo_root().join(relative);
        let content =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            content.contains(&format!("memhub render --actor {actor}")),
            "{} must attribute wrap-up renders",
            path.display()
        );
        assert!(
            content.contains("If render fails") && content.contains("before sync"),
            "{} must prevent sync after a failed render",
            path.display()
        );
        assert!(
            content.contains("resume at")
                && content.contains("render after fixing the cause")
                && content.contains("do not repeat"),
            "{} must resume after durable writes instead of replaying them",
            path.display()
        );
    }
}

#[test]
fn opencode_wrap_up_requires_verified_session_provenance_before_writes() {
    let path = repo_root().join("templates/skills/opencode/wrap-up/SKILL.md");
    let content = fs::read_to_string(&path).expect("read OpenCode wrap-up skill");

    for required in [
        "opencode2 api get \"/api/session/<current-session-id>\"",
        "require `data.id` to exactly equal",
        "stop before every durable memhub write",
        "session_id",
        "agent_id",
        "provider_id",
        "model_id",
        "variant",
        "--session-id",
        "--agent-id",
        "--provider-id",
        "--model-id",
        "--variant",
        "memhub transcript archive --agent opencode",
        "reuse the exact `session_id` already verified",
    ] {
        assert!(
            content.contains(required),
            "OpenCode wrap-up skill is missing `{required}`"
        );
    }
    assert!(content.contains("--source user+agent:opencode"));
    assert!(content.contains("normalized `opencode` actor and raw client identity"));
    assert!(content.contains("--actor opencode:wrap-up"));
    assert!(content.contains("Do not put these values in the free-form session-note text"));
}

/// True when `s` is a YAML block-scalar indicator on its own: `>` or `|`,
/// optionally followed by chomping (`-`/`+`) and/or an explicit indentation
/// digit, and nothing else. Anything past that is the block's own
/// (indented, continuation-line) content, not part of the indicator.
fn is_block_scalar_indicator(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some('>') | Some('|') => {}
        _ => return false,
    }
    chars.all(|c| c == '-' || c == '+' || c.is_ascii_digit())
}

/// True when `s` is entirely wrapped in matching single or double quotes,
/// i.e. a quoted YAML scalar rather than a bare plain scalar.
fn is_safely_quoted(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'')
}

/// Guards the F13 regression class: task 77 appended `Trigger on:
/// "..."` phrase lists into skill frontmatter `description:` fields as
/// plain, unquoted YAML scalars. A plain scalar containing `": "`
/// (colon-space) is invalid YAML — most parsers respond by silently
/// dropping the whole `description` field rather than raising an error,
/// so four high-traffic skills (recall/locate/metrics/doc) lost their
/// routing description on all three agents with nothing failing a
/// build. This test intentionally does not depend on a YAML parser
/// (memhub does not pull in serde_yaml or any other yaml crate); it
/// only checks the one shape that broke: a plain or quoted
/// `description:` scalar must not contain a bare `": "` unless it is
/// switched to a block-scalar form (`description: >` / `description:
/// |`) or the whole value is wrapped in matching quotes.
#[test]
fn skill_frontmatter_descriptions_are_valid_yaml_scalars() {
    let mut failures = Vec::new();

    for path in all_skill_template_files() {
        let content =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        let mut lines = content.lines();
        if lines.next() != Some("---") {
            failures.push(format!(
                "{}: does not start with a `---` frontmatter delimiter",
                path.display()
            ));
            continue;
        }

        let mut frontmatter: Vec<&str> = Vec::new();
        let mut closed = false;
        for line in lines {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            frontmatter.push(line);
        }
        if !closed {
            failures.push(format!(
                "{}: frontmatter has no closing `---`",
                path.display()
            ));
            continue;
        }

        let Some(description_line) = frontmatter.iter().find(|l| l.starts_with("description:"))
        else {
            failures.push(format!(
                "{}: frontmatter has no `description:` key",
                path.display()
            ));
            continue;
        };

        let inline = description_line["description:".len()..].trim();

        if is_block_scalar_indicator(inline) || is_safely_quoted(inline) {
            continue;
        }

        if inline.contains(": ") {
            failures.push(format!(
                "{}: `description:` is a plain scalar containing `\": \"`, \
                 which is invalid YAML (the parser drops the whole field) — \
                 wrap it in a block scalar (`description: >`) or quote the \
                 whole value. Offending value: {inline:?}",
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "skill frontmatter `description:` scalars are invalid YAML:\n{}",
        failures.join("\n")
    );
}

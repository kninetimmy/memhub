//! Contract test: the Claude, Codex, and OpenCode agent surfaces must stay in
//! parity, and the README install blocks plus the two tracked
//! orientation files must not drift away from the actual skill set.
//!
//! Metrics and viz templates intentionally remain in parity as dormant
//! reactivation assets, but default install blocks exclude them while the
//! subsystem is hibernated.
//!
//! There is no CI in this repo — `cargo test` is the gate. Adding a
//! new skill, or a new `## ` section to one orientation file, now
//! forces the matching update everywhere or this test fails.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

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

/// OpenCode skills are `templates/skills/opencode/<name>/SKILL.md`.
fn opencode_skill_names() -> BTreeSet<String> {
    dir_per_skill_names("templates/skills/opencode")
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
    set.remove("viz");
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
/// `templates/skills/opencode/*/SKILL.md`.
fn all_skill_template_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    let claude_dir = repo_root().join("templates/skills/claude");
    for entry in fs::read_dir(&claude_dir).expect("read templates/skills/claude") {
        let path = entry.expect("read dir entry").path();
        if path.extension().and_then(|x| x.to_str()) == Some("md") {
            files.push(path);
        }
    }

    for relative in ["templates/skills/codex", "templates/skills/opencode"] {
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

//! Contract test: the Claude and Codex agent surfaces must stay in
//! parity, and the README install blocks plus the two tracked
//! orientation files must not drift away from the actual skill set.
//!
//! This exists because the surface drifted once already: `/metrics`
//! and `/viz` shipped as Codex skill templates and as live Claude
//! commands but were never tracked under `templates/skills/claude/`,
//! the README install blocks omitted them from the prose enumeration,
//! and `AGENTS.md` was missing the `## Token Accounting` section that
//! `CLAUDE.md` carried. Nothing failed a build, so nobody noticed.
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

/// Skills that are intentionally only on one side. Empty today — both
/// agents expose the identical set. A future intentional divergence
/// goes here *with a comment*, so "we meant that" is explicit and the
/// reviewer sees it in the diff rather than the test silently passing.
const CLAUDE_ONLY_SKILLS: &[&str] = &[];
const CODEX_ONLY_SKILLS: &[&str] = &[];

/// `## ` headers that intentionally exist in only one orientation
/// file. `AGENTS.md` carries a Codex-specific attribution section that
/// has no Claude counterpart by design.
const AGENTS_ONLY_SECTIONS: &[&str] = &["Agent attribution (Codex-specific)"];
const CLAUDE_ONLY_SECTIONS: &[&str] = &[];

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

/// The canonical skill set both agents must expose: every skill that
/// is not on an allowlist must appear on both sides.
fn canonical_skill_set() -> BTreeSet<String> {
    let mut set = claude_skill_names();
    set.extend(codex_skill_names());
    for s in CLAUDE_ONLY_SKILLS.iter().chain(CODEX_ONLY_SKILLS.iter()) {
        set.remove(*s);
    }
    set
}

#[test]
fn claude_and_codex_skill_template_sets_match() {
    let claude = claude_skill_names();
    let codex = codex_skill_names();

    assert!(!claude.is_empty(), "no Claude skill templates discovered");
    assert!(!codex.is_empty(), "no Codex skill templates discovered");

    let allowed_claude_only: BTreeSet<String> =
        CLAUDE_ONLY_SKILLS.iter().map(|s| s.to_string()).collect();
    let allowed_codex_only: BTreeSet<String> =
        CODEX_ONLY_SKILLS.iter().map(|s| s.to_string()).collect();

    let claude_only: BTreeSet<_> = claude.difference(&codex).cloned().collect();
    let codex_only: BTreeSet<_> = codex.difference(&claude).cloned().collect();

    assert_eq!(
        claude_only, allowed_claude_only,
        "skills present only under templates/skills/claude/ (add the Codex \
         SKILL.md counterpart, or allowlist it in CLAUDE_ONLY_SKILLS with a reason)"
    );
    assert_eq!(
        codex_only, allowed_codex_only,
        "skills present only under templates/skills/codex/ (add the Claude \
         .md counterpart, or allowlist it in CODEX_ONLY_SKILLS with a reason)"
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
                && (bytes[j].is_ascii_lowercase()
                    || bytes[j].is_ascii_digit()
                    || bytes[j] == b'-')
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
    let readme =
        fs::read_to_string(repo_root().join("README.md")).expect("read README.md");

    // Both the Claude and Codex install blocks carry one stable
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
        2,
        "expected exactly two install-block skill enumerations \
         (Claude + Codex); found {}",
        segments.len()
    );

    let canonical = canonical_skill_set();
    for (idx, seg) in segments.iter().enumerate() {
        let listed = slash_tokens(seg);
        assert_eq!(
            listed, canonical,
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

/// Extract `## ` section headers in document order.
fn section_headers(md: &str) -> Vec<String> {
    md.lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(|s| s.trim().to_string())
        .collect()
}

#[test]
fn claude_md_and_agents_md_sections_stay_in_parity() {
    let claude = fs::read_to_string(repo_root().join("CLAUDE.md")).expect("read CLAUDE.md");
    let agents = fs::read_to_string(repo_root().join("AGENTS.md")).expect("read AGENTS.md");

    let claude_sections: BTreeSet<String> = section_headers(&claude).into_iter().collect();
    let agents_sections: BTreeSet<String> = section_headers(&agents).into_iter().collect();

    let allowed_agents_only: BTreeSet<String> =
        AGENTS_ONLY_SECTIONS.iter().map(|s| s.to_string()).collect();
    let allowed_claude_only: BTreeSet<String> =
        CLAUDE_ONLY_SECTIONS.iter().map(|s| s.to_string()).collect();

    let claude_only: BTreeSet<_> = claude_sections.difference(&agents_sections).cloned().collect();
    let agents_only: BTreeSet<_> = agents_sections.difference(&claude_sections).cloned().collect();

    assert_eq!(
        claude_only, allowed_claude_only,
        "## sections in CLAUDE.md with no AGENTS.md counterpart (mirror the \
         section into AGENTS.md, or allowlist it in CLAUDE_ONLY_SECTIONS \
         with a reason)"
    );
    assert_eq!(
        agents_only, allowed_agents_only,
        "## sections in AGENTS.md with no CLAUDE.md counterpart (mirror the \
         section into CLAUDE.md, or allowlist it in AGENTS_ONLY_SECTIONS \
         with a reason)"
    );
}

//! Parse the versioned "managed block" that pins how memhub is wired into a
//! consumer repo's root orientation file (`CLAUDE.md`, and by extension
//! `AGENTS.md` once generated from it — issue #31 / decision Q23).
//!
//! The block is a small, hand-authored (not generated) pointer bracketed by
//! two HTML comments:
//!
//! ```text
//! <!-- memhub:managed-block v=1 -->
//! memhub-primary: true
//! db: .memhub/project.sqlite
//! rendered: .memhub/rendered/
//! config: .memhub/config.toml
//! <!-- /memhub:managed-block -->
//! ```
//!
//! It exists so tooling (`memhub audit md`, issue #32 / C5) can check
//! precisely how memhub is wired into a repo instead of grepping prose.
//!
//! ## Namespacing vs the pre-existing `delegation-policy` block (review
//! finding P21)
//!
//! `CLAUDE.md` already carries an unrelated HTML-comment block around its
//! Delegation section: `<!-- BEGIN MANAGED: delegation-policy ... -->` /
//! `<!-- END MANAGED: delegation-policy -->`. Review finding P21 flagged
//! that block as "foreign" to any parser — it is free-form, human-authored
//! policy prose with no maintaining code (a repo-wide grep at the time
//! turned up zero logic that reads it). This module's block is the
//! opposite: a real, versioned, code-parsed contract, so it deliberately
//! does **not** reuse the `BEGIN MANAGED: <name>` / `END MANAGED: <name>`
//! (English prose) convention. Instead it uses an XML/HTML-tag-style
//! open/close pair, `memhub:managed-block` / `/memhub:managed-block`,
//! namespaced under `memhub:` so:
//!
//! - It cannot collide with `MANAGED: delegation-policy`, even under a
//!   loose scan — the substring `memhub:managed-block` never appears in
//!   that block, and vice versa.
//! - The version lives directly in the open tag (`v=1`) rather than being
//!   shoehorned into the English "per-repo; delete this whole block to
//!   revert" prose style, which has no room for a machine-checked field.
//! - The tag style itself signals "code owns and parses this" the moment a
//!   reader sees it, distinct from "this is an editable policy section" —
//!   matching how differently the two are actually maintained.
//!
//! `parse_managed_block` only ever matches on the `memhub:managed-block`
//! substring, so it is structurally immune to matching the
//! `delegation-policy` block (see `ignores_the_foreign_delegation_policy_block`
//! below).

use std::collections::BTreeMap;

/// The managed-block format version this build emits and expects. Bump this
/// when the field set or syntax changes in a way `memhub audit md` needs to
/// tell apart from older repos it scans (its A2 check).
pub const MANAGED_BLOCK_VERSION: u32 = 1;

const OPEN_PREFIX: &str = "<!-- memhub:managed-block v=";
const CLOSE_TAG: &str = "<!-- /memhub:managed-block -->";

/// A parsed managed block: its version tag plus the `key: value` pointer
/// fields found between the open and close markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedBlock {
    pub version: u32,
    pub fields: BTreeMap<String, String>,
}

impl ManagedBlock {
    /// Look up a pointer field by key (e.g. `"db"`).
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

/// Parse the first `memhub:managed-block` found in `md`, if any.
///
/// Returns `None` when no open marker is present, or when the block is
/// malformed (unterminated, or a non-numeric version) — both read as "no
/// usable managed block" to a caller; `memhub audit md` can still report
/// the more precise reason if it needs to by re-scanning for `OPEN_PREFIX`
/// on its own. Line-ending-agnostic: `\r\n` and `\n` both parse (this repo
/// checks out CRLF on Windows under `core.autocrlf=true`).
pub fn parse_managed_block(md: &str) -> Option<ManagedBlock> {
    let md = md.replace("\r\n", "\n");

    let open_start = md.find(OPEN_PREFIX)?;
    let after_prefix = &md[open_start + OPEN_PREFIX.len()..];
    let open_end = after_prefix.find("-->")?;
    let version: u32 = after_prefix[..open_end].trim().parse().ok()?;

    let body_start = open_start + OPEN_PREFIX.len() + open_end + "-->".len();
    let close_rel = md[body_start..].find(CLOSE_TAG)?;
    let body = &md[body_start..body_start + close_rel];

    let mut fields = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    Some(ManagedBlock { version, fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# memhub

Intro.

<!-- memhub:managed-block v=1 -->
memhub-primary: true
db: .memhub/project.sqlite
rendered: .memhub/rendered/
config: .memhub/config.toml
<!-- /memhub:managed-block -->

## Session Continuity
";

    #[test]
    fn parses_a_well_formed_block() {
        let block = parse_managed_block(SAMPLE).expect("block present");
        assert_eq!(block.version, 1);
        assert_eq!(block.field("memhub-primary"), Some("true"));
        assert_eq!(block.field("db"), Some(".memhub/project.sqlite"));
        assert_eq!(block.field("rendered"), Some(".memhub/rendered/"));
        assert_eq!(block.field("config"), Some(".memhub/config.toml"));
        assert_eq!(block.field("no-such-field"), None);
    }

    #[test]
    fn returns_none_when_absent() {
        assert_eq!(
            parse_managed_block("# just a heading\nno block here\n"),
            None
        );
    }

    #[test]
    fn returns_none_when_unterminated() {
        let md = "<!-- memhub:managed-block v=1 -->\nkey: value\n";
        assert_eq!(parse_managed_block(md), None);
    }

    #[test]
    fn returns_none_on_non_numeric_version() {
        let md =
            "<!-- memhub:managed-block v=abc -->\nkey: value\n<!-- /memhub:managed-block -->\n";
        assert_eq!(parse_managed_block(md), None);
    }

    /// Structural proof of the P21 namespacing writeup above: a document
    /// that carries only the pre-existing `delegation-policy` block, and no
    /// `memhub:managed-block`, must parse to `None`, not accidentally match.
    #[test]
    fn ignores_the_foreign_delegation_policy_block() {
        let md = "\
<!-- BEGIN MANAGED: delegation-policy (per-repo; delete this whole block to revert) -->
## Delegation
Some policy prose that is not a memhub managed block.
<!-- END MANAGED: delegation-policy -->
";
        assert_eq!(parse_managed_block(md), None);
    }

    #[test]
    fn is_line_ending_agnostic() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(parse_managed_block(SAMPLE), parse_managed_block(&crlf));
    }
}

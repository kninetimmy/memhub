//! Language → grammar registry for the AST chunker (M11 PR2, decision 107).
//!
//! A deliberately small seam: each supported language maps to a
//! tree-sitter [`Language`] plus the set of top-level node kinds the
//! chunker emits a symbol chunk for. v1 ships **Rust only**; adding a
//! language is one [`GrammarSpec`] row here plus the matching grammar
//! crate in `Cargo.toml`. A language with no row falls back to the
//! line-window chunker (the same path a parse failure takes), so an
//! unsupported file is never silently dropped.
//!
//! The `language` keys match [`super::infer_language`]'s extension map.

use tree_sitter::Language;

/// A tree-sitter grammar plus the node-kind vocabulary the chunker keys
/// off. Kinds are matched against `node.kind()` while walking the tree.
pub struct GrammarSpec {
    /// The tree-sitter language/grammar handle.
    pub language: Language,
    /// Node kind of a free function definition (module- or block-level).
    pub function_kind: &'static str,
    /// Node kind of an `impl` block; its method `function_kind` children
    /// are emitted as `Type::method` chunks rather than the whole block.
    pub impl_kind: &'static str,
    /// Node kinds emitted as a single self-contained symbol chunk
    /// (structs, enums, traits, …). Small enough to embed whole.
    pub item_kinds: &'static [&'static str],
    /// Node kind of an inline module (`mod foo { … }`); recursed into so
    /// nested items are still chunked. Items in an out-of-line `mod foo;`
    /// live in their own file and are walked when that file is indexed.
    pub module_kind: &'static str,
    /// Node kind of a line comment (`//`, `///`, `//!`). Contiguous
    /// preceding doc comments are folded into the following item's chunk.
    pub line_comment_kind: &'static str,
    /// Node kind of a block comment (`/* */`, `/** */`).
    pub block_comment_kind: &'static str,
    /// Node kind of an outer attribute (`#[...]`). A contiguous run
    /// preceding an item is folded into its chunk alongside doc comments.
    pub attribute_kind: &'static str,
}

/// The grammar for `language` (as keyed by [`super::infer_language`]), or
/// `None` when no grammar is registered — the caller then line-windows.
pub fn grammar_for(language: Option<&str>) -> Option<GrammarSpec> {
    match language? {
        "rust" => Some(GrammarSpec {
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kind: "function_item",
            impl_kind: "impl_item",
            item_kinds: &[
                "struct_item",
                "enum_item",
                "union_item",
                "trait_item",
                "type_item",
                "const_item",
                "static_item",
                "macro_definition",
            ],
            module_kind: "mod_item",
            line_comment_kind: "line_comment",
            block_comment_kind: "block_comment",
            attribute_kind: "attribute_item",
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_grammar_is_registered_and_loadable() {
        let spec = grammar_for(Some("rust")).expect("rust grammar present");
        // A parser must accept the language without an ABI-version panic;
        // this is the canary for a tree-sitter / grammar version skew.
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&spec.language)
            .expect("set rust language (ABI compatible)");
        let tree = parser.parse("fn main() {}", None).expect("parse");
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn unknown_and_absent_languages_have_no_grammar() {
        assert!(grammar_for(Some("cobol")).is_none());
        assert!(grammar_for(Some("markdown")).is_none());
        assert!(grammar_for(None).is_none());
    }
}

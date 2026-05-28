//! Language → grammar registry for the AST chunker (M11, decisions
//! 107 and 115).
//!
//! Each supported language maps to a tree-sitter [`Language`] plus a
//! declarative [`GrammarSpec`]: role sets naming the node kinds the
//! walker keys off, and three typed hooks for the handful of per-language
//! quirks the role sets cannot express. The hooks' `Standard` /
//! `Direct` / `PrecedingSiblings` defaults reproduce Rust exactly, so a
//! conventional language is one all-default row.
//!
//! v1 of the multi-language rollout (decision 115) lands the spec shape
//! and the generalized walker while still shipping **Rust only**; the
//! other five grammars (C#, Java, TypeScript, JavaScript, Python, Go)
//! arrive in later tasks as additional rows. A language with no row falls
//! back to the line-window chunker (the same path a parse failure takes),
//! so an unsupported file is never silently dropped.
//!
//! The `language` keys match [`super::infer_language`]'s extension map.

use tree_sitter::Language;

/// How a method's symbol prefix is derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodNaming {
    /// Prefix from the enclosing container: an `impl`/class `prefix_field`
    /// (Rust `impl` `type`) or a containing type's name. Yields
    /// `Prefix::method`. The Rust default.
    Standard,
    /// Go: the prefix comes from the method's own receiver parameter, not
    /// an enclosing container. Implemented in the Go task (T5).
    GoReceiver,
}

/// How a free function's base name is derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionNaming {
    /// Name from the node's `name` field. The Rust default.
    Direct,
    /// JS/TS: an arrow or function expression bound to a
    /// `variable_declarator` takes the declarator's name. Implemented in
    /// the TypeScript/JavaScript task (T3).
    JsDeclarator,
}

/// How preceding documentation is folded into an item's chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocFold {
    /// Fold a contiguous run of preceding comment / attribute siblings
    /// into the item's chunk. The Rust default.
    PrecedingSiblings,
    /// Python: ignore preceding `#` comments, keep the leading body
    /// docstring inside the header chunk, and climb a
    /// `decorated_definition` to fold decorators. Implemented in the
    /// Python task (T4).
    PythonDocstring,
    /// No doc folding; a chunk starts at the item node itself.
    None,
}

/// A method-bearing container: a node kind plus the field its member
/// methods take their type prefix from (Rust `impl_item` → `type`).
pub struct MethodContainer {
    /// Node kind of the container.
    pub node: &'static str,
    /// Field whose text is the type prefix for the container's methods.
    pub prefix_field: &'static str,
}

/// A tree-sitter grammar plus the node-kind vocabulary and per-language
/// hooks the walker keys off. Kinds are matched against `node.kind()`.
pub struct GrammarSpec {
    /// The tree-sitter language/grammar handle.
    pub language: Language,
    /// Node kinds of a free function definition (module- or block-level).
    /// Inside a [`MethodContainer`] body these are emitted as methods.
    pub function_kinds: &'static [&'static str],
    /// Node kinds of a type that contains members (classes, etc.): emitted
    /// as a header-only chunk and recursed into ([`body_field`]) with a
    /// name-derived prefix, nesting qualified. Empty for Rust, whose
    /// structs/enums are leaf [`item_kinds`]. Walker support lands in T2.
    ///
    /// [`body_field`]: GrammarSpec::body_field
    pub type_container_kinds: &'static [&'static str],
    /// Method-bearing containers whose member functions are emitted as
    /// `Prefix::method`, the prefix read from each entry's `prefix_field`
    /// (Rust `impl`).
    pub method_containers: &'static [MethodContainer],
    /// Namespace / inline-module kinds: recursed into via [`body_field`]
    /// with no chunk of their own (Rust `mod_item`).
    ///
    /// [`body_field`]: GrammarSpec::body_field
    pub namespace_kinds: &'static [&'static str],
    /// Node kinds emitted as a single self-contained symbol chunk
    /// (Rust structs, enums, traits, type/const/static items, macros).
    pub item_kinds: &'static [&'static str],
    /// Member kinds chunked inside a [`type_container_kinds`] with the
    /// type prefix (methods, properties, constructors). Empty for Rust.
    /// Walker support lands in T2.
    ///
    /// [`type_container_kinds`]: GrammarSpec::type_container_kinds
    pub member_kinds: &'static [&'static str],
    /// Comment node kinds folded as documentation ahead of an item
    /// (Rust `line_comment`, `block_comment`).
    pub comment_kinds: &'static [&'static str],
    /// Attribute / annotation node kinds folded ahead of an item
    /// (Rust `attribute_item`).
    pub attribute_kinds: &'static [&'static str],
    /// Field name holding a container's or namespace's body subtree.
    pub body_field: &'static str,
    /// Method-prefix derivation hook.
    pub method_naming: MethodNaming,
    /// Free-function naming hook.
    pub function_naming: FunctionNaming,
    /// Documentation-folding hook.
    pub doc_fold: DocFold,
}

impl GrammarSpec {
    /// Returns `true` when every hook on this spec uses a fully implemented
    /// variant. A spec with an unimplemented hook (e.g.
    /// [`FunctionNaming::JsDeclarator`], [`MethodNaming::GoReceiver`],
    /// [`DocFold::PythonDocstring`]) must return `false` here so `chunk_file`
    /// falls back to line windows instead of reaching a `todo!()` in the
    /// walker. Remove a variant from the guards below once its task lands.
    pub fn hooks_implemented(&self) -> bool {
        matches!(self.function_naming, FunctionNaming::Direct)
            && matches!(self.method_naming, MethodNaming::Standard)
            && matches!(self.doc_fold, DocFold::PrecedingSiblings | DocFold::None)
    }
}

/// The grammar for `language` (as keyed by [`super::infer_language`]), or
/// `None` when no grammar is registered — the caller then line-windows.
pub fn grammar_for(language: Option<&str>) -> Option<GrammarSpec> {
    match language? {
        "rust" => Some(GrammarSpec {
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &["function_item"],
            type_container_kinds: &[],
            method_containers: &[MethodContainer {
                node: "impl_item",
                prefix_field: "type",
            }],
            namespace_kinds: &["mod_item"],
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
            member_kinds: &[],
            comment_kinds: &["line_comment", "block_comment"],
            attribute_kinds: &["attribute_item"],
            body_field: "body",
            method_naming: MethodNaming::Standard,
            function_naming: FunctionNaming::Direct,
            doc_fold: DocFold::PrecedingSiblings,
        }),
        // C# and Java are conventional type-container languages: no free
        // functions, methods/constructors/properties are members of a
        // class/record/interface, and all three hooks take their defaults.
        // Node kinds verified against tree-sitter-c-sharp / -java 0.23.
        "csharp" => Some(GrammarSpec {
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            function_kinds: &[],
            type_container_kinds: &[
                "class_declaration",
                "struct_declaration",
                "interface_declaration",
                "record_declaration",
            ],
            method_containers: &[],
            // Block `namespace X { … }` recurses; `file_scoped_namespace_
            // declaration` has no body, so its types are top-level siblings
            // and are walked directly (it matches no role set and is skipped).
            namespace_kinds: &["namespace_declaration"],
            item_kinds: &["enum_declaration", "delegate_declaration"],
            member_kinds: &[
                "method_declaration",
                "constructor_declaration",
                "property_declaration",
            ],
            // C# has one comment node kind for `//`, `///`, and `/* */`.
            comment_kinds: &["comment"],
            attribute_kinds: &["attribute_list"],
            body_field: "body",
            method_naming: MethodNaming::Standard,
            function_naming: FunctionNaming::Direct,
            doc_fold: DocFold::PrecedingSiblings,
        }),
        "java" => Some(GrammarSpec {
            language: tree_sitter_java::LANGUAGE.into(),
            function_kinds: &[],
            type_container_kinds: &[
                "class_declaration",
                "interface_declaration",
                "record_declaration",
            ],
            method_containers: &[],
            // `package_declaration` has no body; types are top-level
            // siblings, so no namespace recursion is needed.
            namespace_kinds: &[],
            item_kinds: &["enum_declaration", "annotation_type_declaration"],
            member_kinds: &["method_declaration", "constructor_declaration"],
            // Javadoc is a `block_comment` sibling, folded as a doc.
            // Annotations live inside the declaration's `modifiers` node,
            // so they are already within the item's byte range — no
            // attribute set needed.
            comment_kinds: &["line_comment", "block_comment"],
            attribute_kinds: &[],
            body_field: "body",
            method_naming: MethodNaming::Standard,
            function_naming: FunctionNaming::Direct,
            doc_fold: DocFold::PrecedingSiblings,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a trivial snippet through `language`'s grammar and assert the
    /// root node kind — the canary for a tree-sitter / grammar ABI skew.
    fn assert_loads(language: &str, source: &str, expected_root: &str) {
        let spec = grammar_for(Some(language)).expect("grammar present");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&spec.language)
            .expect("set language (ABI compatible)");
        let tree = parser.parse(source, None).expect("parse");
        assert_eq!(tree.root_node().kind(), expected_root);
    }

    #[test]
    fn csharp_grammar_is_registered_and_loadable() {
        assert_loads("csharp", "class C { void M() {} }", "compilation_unit");
    }

    #[test]
    fn java_grammar_is_registered_and_loadable() {
        assert_loads("java", "class C { void m() {} }", "program");
    }

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
    fn rust_row_uses_all_default_hooks() {
        let spec = grammar_for(Some("rust")).expect("rust grammar present");
        assert_eq!(spec.method_naming, MethodNaming::Standard);
        assert_eq!(spec.function_naming, FunctionNaming::Direct);
        assert_eq!(spec.doc_fold, DocFold::PrecedingSiblings);
    }

    #[test]
    fn unknown_and_absent_languages_have_no_grammar() {
        assert!(grammar_for(Some("cobol")).is_none());
        assert!(grammar_for(Some("markdown")).is_none());
        assert!(grammar_for(None).is_none());
    }

    // M11 review L1: every currently registered grammar must report all
    // hooks implemented so no live row can reach a `todo!()` in the walker.
    #[test]
    fn all_registered_grammars_have_fully_implemented_hooks() {
        for lang in ["rust", "csharp", "java"] {
            let spec = grammar_for(Some(lang)).expect(lang);
            assert!(
                spec.hooks_implemented(),
                "{lang} grammar has an unimplemented hook — chunk_file would degrade to line windows"
            );
        }
    }
}

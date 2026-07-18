//! Code chunker (M11, decision 107).
//!
//! Two strategies behind one [`chunk_file`] entry point:
//!
//! * A **tree-sitter AST chunker** ([`chunk_with_grammar`]) for languages
//!   with a registered grammar ([`super::grammar`]). For Rust-like languages
//!   it emits one chunk per top-level item — functions, structs, enums, traits,
//!   type/const/static items, macros — and one chunk per `impl` method (named
//!   `Type::method`). For type-container languages (C#, Java) it emits one
//!   header-only chunk per class/record/interface/struct (named for the type,
//!   with member bodies excised to `{ ... }`) plus one `Type::member` chunk per
//!   method, constructor, and property. Go is neither: free functions and
//!   receiver-named methods (`Type::method`, prefix from the receiver) are
//!   top-level, and a struct/interface/alias/const/var is a leaf chunk. All
//!   paths carry symbol names and kind tags; a contiguous run of preceding doc
//!   comments / attributes is folded into each chunk so the model sees the
//!   documentation with the code.
//!
//! * A **line-window fallback** ([`chunk_line_windows`]) — fixed-size,
//!   symbol-unaware windows — used when the language has no grammar, when
//!   parsing yields no items, or for the bytes of an otherwise unhandled
//!   file. Nothing is ever silently dropped: every non-empty tracked text
//!   file produces at least one chunk.

use tree_sitter::{Node, Parser};

use super::grammar::{
    self, DocFold, FunctionNaming, GrammarSpec, MethodContainer, MethodNaming, ModuleDoc,
};

/// Lines per window. A placeholder value — tuned for retrieval in PR5.
pub const WINDOW_LINES: usize = 50;

/// Hard byte cap on a single line-window chunk body. A window is flushed
/// early once adding the next line would exceed this, and a single line
/// longer than the cap is split on UTF-8 char boundaries into multiple
/// chunks. Without this, a minified or generated single-line file becomes
/// one unbounded chunk that the embedder (a fixed context window) cannot
/// ingest. AST chunks are NOT subject to this cap — an oversized function
/// stays one chunk and the embedder truncates it at its token limit, so a
/// symbol is never split across chunk rows.
pub const MAX_CHUNK_BYTES: usize = 4000;

/// Kind tag stored on every line-window chunk. Distinguishes the fallback
/// chunks from the symbol-aware chunks the AST chunker emits.
pub const LINE_WINDOW_KIND: &str = "line-window";

/// One chunk produced from a source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// 1-indexed inclusive start line.
    pub start_line: usize,
    /// 1-indexed inclusive end line.
    pub end_line: usize,
    /// Symbol name when known: a bare name for free items (`parse_config`),
    /// `Type::method` for impl methods. `None` for line-window chunks.
    pub symbol: Option<String>,
    /// Chunk kind tag: `line-window`, or a symbol kind such as `function`,
    /// `struct`, `enum`, `trait`, `method`.
    pub kind: String,
    /// Text fed to the embedder / FTS. Line-window: `path` + body. AST:
    /// `path` + `kind name` + body, so the symbol identity is signal.
    pub embed_text: String,
    /// The chunk body, LF-normalized (CRLF→LF). The caller hashes the
    /// `embed_text` (which embeds this) to key the embedding cache, so a
    /// CRLF↔LF re-checkout — which changes the file-level `content_hash`
    /// over raw bytes — must not churn chunk embeddings.
    pub body: String,
}

/// Chunk `content` for the given `language` hint (as keyed by
/// [`super::infer_language`]). Dispatches to the AST chunker when a
/// grammar is registered and it yields symbols; otherwise line-windows.
/// An empty (or whitespace-only) file yields no chunks.
pub fn chunk_file(path: &str, content: &str, language: Option<&str>) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    if let Some(spec) = grammar::grammar_for(language) {
        // A spec whose hooks aren't all implemented degrades to line windows
        // rather than reaching a `todo!()` in the walker. Every currently
        // registered grammar passes; the guard protects a future half-wired row.
        if spec.hooks_implemented() {
            let chunks = chunk_with_grammar(path, content, &spec);
            // A parse that surfaces no items (e.g. a file of only `use`
            // statements, or one too broken to recover symbols) falls through
            // to line windows so its bytes stay searchable — never dropped.
            if !chunks.is_empty() {
                return chunks;
            }
        }
    }
    chunk_line_windows(path, content)
}

/// Walk the AST and emit chunks according to the grammar spec.
/// For Rust-like grammars: one chunk per top-level item or impl method.
/// For type-container grammars (C#/Java): one header chunk per type (member
/// bodies excised) plus one `Type::member` chunk per method/constructor/property.
/// Returns an empty vec when parsing fails or finds no recognized items;
/// the caller then line-windows the file.
fn chunk_with_grammar(path: &str, content: &str, spec: &GrammarSpec) -> Vec<Chunk> {
    let mut parser = Parser::new();
    if parser.set_language(&spec.language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let src = content.as_bytes();
    let mut chunks = Vec::new();

    // Task 85/87: capture the file-level module doc as a `module-doc` chunk
    // before all item chunks. Variant determines the capture strategy.
    match spec.module_doc {
        ModuleDoc::LeadingInnerComments => {
            push_module_doc_chunk(&mut chunks, path, src, tree.root_node())
        }
        ModuleDoc::LeadingFileDoc(markers) => {
            push_leading_comment_doc(&mut chunks, path, src, tree.root_node(), spec, markers)
        }
        ModuleDoc::GoPackageDoc => {
            push_go_package_doc(&mut chunks, path, src, tree.root_node(), spec)
        }
        ModuleDoc::PythonModuleDocstring => {
            push_python_module_docstring(&mut chunks, path, src, tree.root_node())
        }
        ModuleDoc::None => {}
    }

    collect_items(tree.root_node(), src, path, spec, None, &mut chunks);
    chunks
}

/// Scan `root`'s leading named children for a contiguous run of inner-doc
/// comment nodes (`line_comment`/`block_comment` whose text begins with
/// `//!` or `/*!`). A blank-line gap greater than one row between consecutive
/// nodes stops the run. If the run is non-empty, push one `module-doc` chunk
/// covering the whole run. FILE-level only — does not recurse into items.
fn push_module_doc_chunk(out: &mut Vec<Chunk>, path: &str, src: &[u8], root: Node<'_>) {
    let mut run: Vec<Node<'_>> = Vec::new();
    let mut prev_end_row: Option<usize> = None;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let k = child.kind();
        if k != "line_comment" && k != "block_comment" {
            break;
        }
        let text = child.utf8_text(src).unwrap_or("");
        if !text.starts_with("//!") && !text.starts_with("/*!") {
            break;
        }
        // A gap > 1 row between consecutive inner-doc nodes stops the run.
        if let Some(prev_end) = prev_end_row {
            let gap = child.start_position().row as i64 - prev_end as i64;
            if gap > 1 {
                break;
            }
        }
        prev_end_row = Some(child.end_position().row);
        run.push(child);
    }
    if run.is_empty() {
        return;
    }
    let first = run.first().unwrap();
    let last = run.last().unwrap();
    let start_byte = first.start_byte();
    let end_byte = last.end_byte();
    let start_row = first.start_position().row;
    let end_row = last.end_position().row;

    let raw = std::str::from_utf8(&src[start_byte..end_byte]).unwrap_or("");
    let body = raw.replace("\r\n", "\n");
    let embed_text = format!("{path}\nmodule-doc\n\n{body}");

    out.push(Chunk {
        start_line: start_row + 1,
        end_line: end_row + 1,
        symbol: None,
        kind: "module-doc".to_string(),
        embed_text,
        body,
    });
}

/// Shared helper: push a `module-doc` chunk for the byte range
/// `[start_byte, end_byte)` with the given `(start_row, end_row)` (0-indexed).
fn push_doc_chunk(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
) {
    let raw = std::str::from_utf8(&src[start_byte..end_byte]).unwrap_or("");
    let body = raw.replace("\r\n", "\n");
    let embed_text = format!("{path}\nmodule-doc\n\n{body}");
    out.push(Chunk {
        start_line: start_row + 1,
        end_line: end_row + 1,
        symbol: None,
        kind: "module-doc".to_string(),
        embed_text,
        body,
    });
}

/// Collect the leading run of comment nodes (from `spec.comment_kinds`) whose
/// text starts with any of `markers`. A gap > 1 row or a non-matching comment
/// stops the run. Used for C# XML-doc (`///`) and Java/JS/TS JSDoc (`/**`).
///
/// Gap-gated: when the run is immediately adjacent (≤ 1 row) to a following
/// declaration node, the comment is that declaration's doc — already folded
/// into its chunk by `fold_preceding_siblings` — so no module-doc chunk is
/// emitted. Only a blank-line gap, a preamble node (package/using/import), or
/// EOF marks it as a true file-level doc.
fn push_leading_comment_doc(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    root: Node<'_>,
    spec: &GrammarSpec,
    markers: &[&str],
) {
    let mut run: Vec<Node<'_>> = Vec::new();
    let mut prev_end_row: Option<usize> = None;
    let mut after_run: Option<Node<'_>> = None;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let k = child.kind();
        if !spec.comment_kinds.contains(&k) {
            if !run.is_empty() {
                after_run = Some(child);
            }
            break;
        }
        let text = child.utf8_text(src).unwrap_or("");
        if !markers.iter().any(|m| text.starts_with(m)) {
            if !run.is_empty() {
                after_run = Some(child);
            }
            break;
        }
        if let Some(prev_end) = prev_end_row
            && child.start_position().row as i64 - prev_end as i64 > 1
        {
            after_run = Some(child);
            break;
        }
        prev_end_row = Some(child.end_position().row);
        run.push(child);
    }
    if run.is_empty() {
        return;
    }
    // Gap gate: an adjacent following declaration absorbs this comment as its
    // own doc, so it is not a standalone module doc.
    if let Some(next) = after_run {
        let run_last_end = run.last().unwrap().end_position().row;
        let gap = next.start_position().row as i64 - run_last_end as i64;
        if gap <= 1 && is_decl_kind(spec, next.kind()) {
            return;
        }
    }
    let first = run.first().unwrap();
    let last = run.last().unwrap();
    push_doc_chunk(
        out,
        path,
        src,
        first.start_byte(),
        last.end_byte(),
        first.start_position().row,
        last.end_position().row,
    );
}

/// `true` when `kind` is in any declaration role set — the kinds whose
/// preceding doc comment belongs to the declaration, not the file.
fn is_decl_kind(spec: &GrammarSpec, kind: &str) -> bool {
    spec.function_kinds.contains(&kind)
        || spec.type_container_kinds.contains(&kind)
        || spec.item_kinds.contains(&kind)
        || spec.member_kinds.contains(&kind)
        || spec.transparent_kinds.contains(&kind)
}

/// Emit the Go package-level doc: the contiguous `//` comment block that
/// immediately precedes (gap ≤ 1 row) the `package_clause` node. The comment
/// block is the last contiguous run of comment siblings before `package_clause`;
/// if no such block exists (or only unrelated comments with a gap > 1 row), no
/// chunk is emitted.
fn push_go_package_doc(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    root: Node<'_>,
    spec: &GrammarSpec,
) {
    // Walk named children to locate `package_clause` and collect preceding
    // comment nodes. We collect ALL named children in order, then scan backward
    // from `package_clause`.
    let mut cursor = root.walk();
    let children: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
    let pkg_idx = match children.iter().position(|n| n.kind() == "package_clause") {
        Some(i) => i,
        None => return,
    };
    // Walk backward from package_clause collecting a contiguous comment run.
    let mut run: Vec<Node<'_>> = Vec::new();
    let mut next_start_row = children[pkg_idx].start_position().row;
    for node in children[..pkg_idx].iter().rev() {
        let k = node.kind();
        if !spec.comment_kinds.contains(&k) {
            break;
        }
        if (next_start_row as i64 - node.end_position().row as i64) > 1 {
            break;
        }
        next_start_row = node.start_position().row;
        run.push(*node);
    }
    if run.is_empty() {
        return;
    }
    run.reverse();
    let first = run.first().unwrap();
    let last = run.last().unwrap();
    push_doc_chunk(
        out,
        path,
        src,
        first.start_byte(),
        last.end_byte(),
        first.start_position().row,
        last.end_position().row,
    );
}

/// Emit the Python module docstring: the first named child of `root` that is
/// an `expression_statement` whose only child is a `string` node. If present,
/// emit a single `module-doc` chunk covering the statement.
fn push_python_module_docstring(out: &mut Vec<Chunk>, path: &str, src: &[u8], root: Node<'_>) {
    let mut cursor = root.walk();
    let first = root.named_children(&mut cursor).next();
    let stmt = match first {
        Some(n) if n.kind() == "expression_statement" => n,
        _ => return,
    };
    let inner = stmt.named_child(0);
    match inner {
        Some(n) if n.kind() == "string" => {}
        _ => return,
    }
    push_doc_chunk(
        out,
        path,
        src,
        stmt.start_byte(),
        stmt.end_byte(),
        stmt.start_position().row,
        stmt.end_position().row,
    );
}

/// Recursively collect item chunks from `node`'s named children.
/// `type_prefix` is set while inside a method container ([`impl`]) so
/// member functions are named `Type::method`.
fn collect_items(
    node: Node<'_>,
    src: &[u8],
    path: &str,
    spec: &GrammarSpec,
    type_prefix: Option<&str>,
    out: &mut Vec<Chunk>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();

        if spec.function_kinds.contains(&kind) {
            let (symbol, is_method) = function_symbol(&child, src, spec, type_prefix);
            let label = if is_method { "method" } else { "function" };
            push_symbol(out, path, src, spec, child, symbol, label);
        } else if let Some(container) = method_container_for(spec, kind) {
            // Emit the methods, not the whole (possibly huge) container.
            let prefix = field_text(&child, container.prefix_field, src);
            if let Some(body) = child.child_by_field_name(spec.body_field) {
                collect_items(body, src, path, spec, prefix, out);
            }
        } else if spec.type_container_kinds.contains(&kind) {
            // A class/record/interface: emit a header-only chunk (member
            // and nested-type bodies excised) and recurse into the body so
            // each member becomes its own `Type::member` chunk.
            let name = field_text(&child, "name", src);
            let qualified = qualify(type_prefix, name);
            push_type_header(
                out,
                path,
                src,
                spec,
                child,
                qualified.clone(),
                kind_label(kind),
            );
            if let Some(body) = child.child_by_field_name(spec.body_field) {
                collect_items(body, src, path, spec, qualified.as_deref(), out);
            }
        } else if spec.namespace_kinds.contains(&kind) {
            // Recurse into an inline namespace / module (`mod foo { … }`);
            // an out-of-line `mod foo;` has no body here and is indexed
            // via its own file. Namespaces do not qualify member names.
            if let Some(body) = child.child_by_field_name(spec.body_field) {
                collect_items(body, src, path, spec, type_prefix, out);
            }
        } else if spec.transparent_kinds.contains(&kind) {
            // A structural wrapper (JS/TS `export_statement`, or the
            // `expression_statement` around a TS namespace): emit no chunk of
            // its own, just walk its named children at the same prefix so the
            // wrapped declaration is reached. Doc comments preceding the
            // wrapper still fold in — `leading_start` climbs back through it.
            collect_items(child, src, path, spec, type_prefix, out);
        } else if spec.item_kinds.contains(&kind) {
            let symbol = qualify(type_prefix, field_text(&child, "name", src));
            push_symbol(out, path, src, spec, child, symbol, kind_label(kind));
        } else if type_prefix.is_some() && spec.member_kinds.contains(&kind) {
            // A method/constructor/property inside a type container.
            // Note: body-less members (interface/abstract methods, C# auto-
            // properties like `int X { get; set; }`) intentionally appear
            // BOTH here as their own chunk AND verbatim in the parent type's
            // header chunk (slice_header only excises nodes that have a
            // body_field child). The duplication improves recall: the header
            // chunk gives the member context alongside its siblings, while
            // the individual chunk makes it directly addressable by symbol.
            let symbol = qualify(type_prefix, field_text(&child, "name", src));
            push_symbol(out, path, src, spec, child, symbol, kind_label(kind));
        } else if uses_js_declarator(spec)
            && matches!(kind, "lexical_declaration" | "variable_declaration")
        {
            // JS/TS: a `const foo = () => {}` / function-expression binding.
            // The name lives on the parent declarator, not the function node
            // (the JsDeclarator hook). Non-function bindings (`const x = 5`)
            // produce no chunk.
            push_declarator_functions(out, path, src, spec, child, type_prefix);
        } else if uses_js_declarator(spec)
            && type_prefix.is_some()
            && matches!(kind, "public_field_definition" | "field_definition")
            && is_function_valued(&child)
        {
            // JS/TS class-field arrow method: `handle = () => {}`. Named from
            // the field's name (TS `public_field_definition` exposes it as
            // `name`, JS `field_definition` as `property`), qualified to
            // `Type::handle`. Plain (non-function) fields fall through and
            // stay in the type header only.
            let name =
                field_text(&child, "name", src).or_else(|| field_text(&child, "property", src));
            let symbol = qualify(type_prefix, name);
            push_symbol(out, path, src, spec, child, symbol, "method");
        }
    }
}

/// `true` when the grammar derives free-function names via their binding
/// declarator (JS/TS), so the walker unwraps `lexical_declaration` /
/// `variable_declaration` and arrow-valued class fields.
fn uses_js_declarator(spec: &GrammarSpec) -> bool {
    matches!(spec.function_naming, FunctionNaming::JsDeclarator)
}

/// `true` when `node`'s `value` field is a function/arrow expression, i.e.
/// the binding is a function definition rather than a data binding.
fn is_function_valued(node: &Node<'_>) -> bool {
    node.child_by_field_name("value")
        .map(|v| {
            matches!(
                v.kind(),
                "arrow_function" | "function" | "function_expression" | "generator_function"
            )
        })
        .unwrap_or(false)
}

/// Emit a free-function chunk for each function-valued `variable_declarator`
/// in a JS/TS `lexical_declaration` / `variable_declaration`. A lone
/// declarator chunks the whole statement (keeping `const`/`let`/`export`);
/// in a multi-binding statement (`let a = 1, f = () => …`) each function
/// declarator is chunked on its own so a data sibling is not mislabeled.
fn push_declarator_functions(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    spec: &GrammarSpec,
    decl: Node<'_>,
    type_prefix: Option<&str>,
) {
    let mut cursor = decl.walk();
    let declarators: Vec<Node<'_>> = decl
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "variable_declarator")
        .collect();
    let lone = declarators.len() == 1;
    for d in declarators {
        if !is_function_valued(&d) {
            continue;
        }
        let symbol = qualify(type_prefix, field_text(&d, "name", src));
        let label = if type_prefix.is_some() {
            "method"
        } else {
            "function"
        };
        let node = if lone { decl } else { d };
        push_symbol(out, path, src, spec, node, symbol, label);
    }
}

/// Qualify `name` with a `Type::` prefix when inside a container, using the
/// canonical `::` separator for every language. `None` name → `None`.
fn qualify(type_prefix: Option<&str>, name: Option<&str>) -> Option<String> {
    match (type_prefix, name) {
        (Some(ty), Some(n)) => Some(format!("{ty}::{n}")),
        (None, Some(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// The [`MethodContainer`] registered for `kind`, if any.
fn method_container_for<'a>(spec: &'a GrammarSpec, kind: &str) -> Option<&'a MethodContainer> {
    spec.method_containers.iter().find(|c| c.node == kind)
}

/// Resolve a function node's `(symbol, is_method)`, honoring the
/// `function_naming` and `method_naming` hooks. Returns whether the node is a
/// method so the caller can label it — for `Standard` that is simply "inside a
/// container" (`type_prefix.is_some()`), but Go methods are top-level
/// `method_declaration`s with no enclosing prefix, so `GoReceiver` decides it
/// from the node's own receiver.
fn function_symbol(
    func: &Node<'_>,
    src: &[u8],
    spec: &GrammarSpec,
    type_prefix: Option<&str>,
) -> (Option<String>, bool) {
    let base = match spec.function_naming {
        // A named `function foo` reads its `name` field directly; this branch
        // handles both Rust-style `Direct` and the JS/TS `function_declaration`
        // case. Arrow/function bindings (`const foo = () => {}`) carry their
        // name on the declarator and are walked separately in
        // `push_declarator_functions`, never reaching here.
        FunctionNaming::Direct | FunctionNaming::JsDeclarator => {
            field_text(func, "name", src).map(str::to_string)
        }
    };
    match spec.method_naming {
        // Qualified only inside a method container (Rust `impl`, C#/Java class).
        MethodNaming::Standard => match type_prefix {
            Some(ty) => (base.map(|n| format!("{ty}::{n}")), true),
            None => (base, false),
        },
        // Go: the prefix is the receiver's type, read from the node. A node
        // with no receiver is a free function.
        MethodNaming::GoReceiver => match go_receiver_type(func, src) {
            Some(ty) => (base.map(|n| format!("{ty}::{n}")), true),
            None => (base, false),
        },
    }
}

/// The receiver type name of a Go `method_declaration`, stripped of a leading
/// pointer (`*Point` → `Point`) and any generic parameters (`Stack[T]` →
/// `Stack`). `None` for a `function_declaration` (no `receiver` field) or an
/// unexpected shape — the caller then treats the node as a free function.
fn go_receiver_type<'a>(method: &Node<'_>, src: &'a [u8]) -> Option<&'a str> {
    let receiver = method.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parameter_declaration")?;
    let mut ty = param.child_by_field_name("type")?;
    // `*T` — the pointed-to type is the pointer node's only named child.
    if ty.kind() == "pointer_type" {
        ty = ty.named_child(0)?;
    }
    // `T[…]` — the base type is the generic node's `type` field.
    if ty.kind() == "generic_type" {
        ty = ty.child_by_field_name("type")?;
    }
    ty.utf8_text(src).ok()
}

/// Build and push one symbol chunk for `item`, folding any contiguous
/// preceding doc comments / attributes into its line range and body.
fn push_symbol(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    spec: &GrammarSpec,
    item: Node<'_>,
    symbol: Option<String>,
    label: &str,
) {
    let (start_byte, start_row) = leading_start(item, spec);
    let end_byte = item.end_byte();
    let end_row = item.end_position().row;

    // Slice the original source over the doc-comment-extended range. The
    // offsets come from nodes parsed from this same buffer, so the range is
    // always valid UTF-8 on a char boundary.
    let raw = std::str::from_utf8(&src[start_byte..end_byte]).unwrap_or("");
    let body = raw.replace("\r\n", "\n");
    let name = symbol.as_deref().unwrap_or(label);
    let embed_text = format!("{path}\n{label} {name}\n\n{body}");

    out.push(Chunk {
        start_line: start_row + 1,
        end_line: end_row + 1,
        symbol,
        kind: label.to_string(),
        embed_text,
        body,
    });
}

/// Push a header-only chunk for a type container: its signature, fields,
/// and folded doc, with the body of each direct member (method bodies,
/// nested-type bodies) excised to a `{ ... }` placeholder. The member and
/// nested-type bodies live in their own chunks, so this gives a class-level
/// query a home without duplicating them.
fn push_type_header(
    out: &mut Vec<Chunk>,
    path: &str,
    src: &[u8],
    spec: &GrammarSpec,
    container: Node<'_>,
    symbol: Option<String>,
    label: &str,
) {
    let (start_byte, start_row) = leading_start(container, spec);
    let end_byte = container.end_byte();
    let end_row = container.end_position().row;

    let body = slice_header(container, src, start_byte, end_byte, spec);
    let name = symbol.as_deref().unwrap_or(label);
    let embed_text = format!("{path}\n{label} {name}\n\n{body}");

    out.push(Chunk {
        start_line: start_row + 1,
        end_line: end_row + 1,
        symbol,
        kind: label.to_string(),
        embed_text,
        body,
    });
}

/// Build a type container's header text over `[start_byte, end_byte)`
/// (which includes any folded leading doc): copy the source verbatim but
/// replace the `body_field` subtree of each direct member with `{ ... }`.
/// CRLF is normalized to LF, matching [`push_symbol`].
fn slice_header(
    container: Node<'_>,
    src: &[u8],
    start_byte: usize,
    end_byte: usize,
    spec: &GrammarSpec,
) -> String {
    // Offsets are relative to `start_byte` so the leading doc (parsed from
    // this same buffer) is preserved ahead of the container node.
    let mut excise: Vec<(usize, usize)> = Vec::new();
    if let Some(body) = container.child_by_field_name(spec.body_field) {
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            collect_member_excisions(member, spec, start_byte, end_byte, &mut excise);
        }
    }
    excise.sort_unstable();

    let full = std::str::from_utf8(&src[start_byte..end_byte]).unwrap_or("");
    let mut header = String::with_capacity(full.len());
    let mut pos = 0usize;
    for (s, e) in excise {
        // Clamp to `full.len()` and enforce monotonicity before indexing.
        let s = s.min(full.len()).max(pos);
        let e = e.min(full.len()).max(s);
        header.push_str(&full[pos..s]);
        header.push_str("{ ... }");
        pos = e;
    }
    header.push_str(&full[pos..]);
    header.replace("\r\n", "\n")
}

/// Record the byte range of `member`'s body subtree (relative to
/// `start_byte`) for excision from a type header, descending through a
/// transparent wrapper (Python `decorated_definition`) to reach the real
/// definition's body so a decorated method's body is excised like a plain
/// one. Body-less members — assignments, a class docstring's
/// `expression_statement`, interface/abstract methods, C# auto-properties,
/// and the `decorator` nodes themselves — have no `body_field` child and
/// contribute nothing, so they stay verbatim in the header. (collect_items
/// still emits the methods as their own chunks; the duplication of a
/// body-less member is intentional — see the member_kinds branch.)
fn collect_member_excisions(
    member: Node<'_>,
    spec: &GrammarSpec,
    start_byte: usize,
    end_byte: usize,
    excise: &mut Vec<(usize, usize)>,
) {
    if spec.transparent_kinds.contains(&member.kind()) {
        let mut cursor = member.walk();
        for inner in member.named_children(&mut cursor) {
            collect_member_excisions(inner, spec, start_byte, end_byte, excise);
        }
        return;
    }
    if let Some(inner) = member.child_by_field_name(spec.body_field) {
        let s = inner.start_byte();
        let e = inner.end_byte();
        // Only record when both offsets are within [start_byte, end_byte].
        if s >= start_byte && e <= end_byte && s <= e {
            excise.push((s - start_byte, e - start_byte));
        }
    }
}

/// The `(start_byte, start_row)` an item's chunk begins at, extended over
/// folded documentation per the grammar's `doc_fold` hook.
fn leading_start(item: Node<'_>, spec: &GrammarSpec) -> (usize, usize) {
    match spec.doc_fold {
        // Fold from the outermost transparent wrapper (JS/TS
        // `export_statement`) so a doc comment above `export const foo` is
        // reached — the comment is a sibling of the wrapper, not of the
        // wrapped declaration. With no transparent kinds (Rust/C#/Java) the
        // anchor stays the item itself, so behavior is unchanged.
        DocFold::PrecedingSiblings => fold_preceding_siblings(fold_anchor(item, spec), spec),
        // Python: do NOT fold preceding `#` comments (they are not docs).
        // Climb out of any `decorated_definition` wrapper (a transparent kind)
        // so the def's decorators are included in its chunk. A class's leading
        // docstring needs no handling here — it lives inside the body and
        // `slice_header` keeps it because an `expression_statement` has no
        // `body_field` to excise.
        DocFold::PythonDocstring => {
            let anchor = fold_anchor(item, spec);
            (anchor.start_byte(), anchor.start_position().row)
        }
        DocFold::None => (item.start_byte(), item.start_position().row),
    }
}

/// Climb out of any transparent wrapper(s) so doc-comment folding starts at
/// the outermost wrapper. Returns `item` unchanged when its parent is not a
/// transparent kind (always the case for Rust/C#/Java).
fn fold_anchor<'a>(item: Node<'a>, spec: &GrammarSpec) -> Node<'a> {
    let mut anchor = item;
    while let Some(parent) = anchor.parent() {
        if spec.transparent_kinds.contains(&parent.kind()) {
            anchor = parent;
        } else {
            break;
        }
    }
    anchor
}

/// Walk backwards from `item` over contiguous preceding comment /
/// attribute siblings, returning the extended `(start_byte, start_row)`.
/// A blank line (a row gap > 1) between siblings stops the run, so an
/// unrelated license header far above is not absorbed.
fn fold_preceding_siblings(item: Node<'_>, spec: &GrammarSpec) -> (usize, usize) {
    let mut earliest = item;
    let mut sib = item.prev_sibling();
    while let Some(s) = sib {
        let k = s.kind();
        let foldable = spec.comment_kinds.contains(&k) || spec.attribute_kinds.contains(&k);
        if !foldable {
            break;
        }
        let gap = earliest.start_position().row as i64 - s.end_position().row as i64;
        if gap > 1 {
            break;
        }
        earliest = s;
        sib = s.prev_sibling();
    }
    (earliest.start_byte(), earliest.start_position().row)
}

fn field_text<'a>(node: &Node<'_>, field: &str, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(src).ok())
}

/// Short kind tag for a tree-sitter node kind: trims a trailing `_item` /
/// `_definition` / `_declaration` / `_signature` / `_spec` so `struct_item` →
/// `struct`, `macro_definition` → `macro`, `method_declaration` → `method`, the
/// TS interface `method_signature` → `method`, and the Go `type_spec` /
/// `const_spec` / `var_spec` → `type` / `const` / `var`. Rust uses none of the
/// `_decl`/`_signature`/`_spec` kinds, so its output is unaffected.
fn kind_label(node_kind: &str) -> &str {
    node_kind
        .strip_suffix("_item")
        .or_else(|| node_kind.strip_suffix("_definition"))
        .or_else(|| node_kind.strip_suffix("_declaration"))
        .or_else(|| node_kind.strip_suffix("_signature"))
        .or_else(|| node_kind.strip_suffix("_spec"))
        .unwrap_or(node_kind)
}

/// Split `content` into fixed line windows. Symbol-unaware fallback. Line
/// numbers are 1-indexed and inclusive.
pub fn chunk_line_windows(path: &str, content: &str) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        // Grow the window up to WINDOW_LINES lines, but flush early once it
        // would cross MAX_CHUNK_BYTES (always taking at least one line so we
        // make progress).
        let mut end = start;
        let mut bytes = 0usize;
        while end < lines.len() && end - start < WINDOW_LINES {
            let line_bytes = lines[end].len() + 1; // +1 for the join newline
            if end > start && bytes + line_bytes > MAX_CHUNK_BYTES {
                break;
            }
            bytes += line_bytes;
            end += 1;
        }

        let body = lines[start..end].join("\n");
        if end == start + 1 && body.len() > MAX_CHUNK_BYTES {
            // A single line longer than the cap (minified/generated file):
            // split it on char boundaries into multiple chunks that all
            // point at the same source line.
            for piece in split_on_byte_budget(&body, MAX_CHUNK_BYTES) {
                push_window(&mut chunks, path, start + 1, start + 1, piece.to_string());
            }
        } else {
            push_window(&mut chunks, path, start + 1, end, body);
        }
        start = end;
    }
    chunks
}

fn push_window(
    chunks: &mut Vec<Chunk>,
    path: &str,
    start_line: usize,
    end_line: usize,
    body: String,
) {
    let embed_text = format!("{path}\n\n{body}");
    chunks.push(Chunk {
        start_line,
        end_line,
        symbol: None,
        kind: LINE_WINDOW_KIND.to_string(),
        embed_text,
        body,
    });
}

/// Split `s` into pieces of at most `max` bytes, never breaking a UTF-8
/// character. `max` must comfortably exceed 4 (the longest UTF-8 char) so
/// the boundary back-off always leaves forward progress.
fn split_on_byte_budget(s: &str, max: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = (start + max).min(s.len());
        while end < s.len() && !s.is_char_boundary(end) {
            end -= 1;
        }
        pieces.push(&s[start..end]);
        start = end;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(content: &str) -> Vec<Chunk> {
        chunk_file("src/lib.rs", content, Some("rust"))
    }

    /// Representative Rust fixture exercising every chunker path: a
    /// module doc comment, leaf items (struct/enum/trait/type/const/
    /// static/macro), a doc+attribute-folded free function, an `impl`
    /// with documented and bare methods, and an inline module with
    /// nested items. The freeze test pins this fixture's chunk output.
    const FREEZE_FIXTURE: &str = r#"//! Module-level doc comment.

use std::fmt;

/// A point in 2D space.
#[derive(Debug)]
pub struct Point {
    x: i32,
    y: i32,
}

/// Cardinal directions.
pub enum Dir {
    North,
    South,
}

/// Greeting behavior.
pub trait Greet {
    fn hello(&self);
}

pub type Coord = (i32, i32);

const MAX: usize = 100;

static NAME: &str = "memhub";

macro_rules! shout {
    () => {};
}

/// Adds two numbers.
#[inline]
fn add(a: i32, b: i32) -> i32 {
    a + b
}

impl Point {
    /// Make a new point.
    pub fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    fn magnitude(&self) -> f64 {
        ((self.x * self.x + self.y * self.y) as f64).sqrt()
    }
}

mod inner {
    fn helper() {}

    struct Cfg;
}
"#;

    /// Freeze the Rust chunker's output (kind, symbol, start_line,
    /// end_line) for [`FREEZE_FIXTURE`]. The M11 multi-language reshape
    /// (decision 115) generalizes [`GrammarSpec`] and the walker; this
    /// snapshot is the regression backbone proving Rust output stays
    /// byte-identical through that work. If a deliberate Rust change
    /// moves these values, update the expected list in the same commit.
    #[test]
    fn rust_chunk_output_is_frozen() {
        let chunks = rust(FREEZE_FIXTURE);
        let got: Vec<(&str, Option<&str>, usize, usize)> = chunks
            .iter()
            .map(|c| {
                (
                    c.kind.as_str(),
                    c.symbol.as_deref(),
                    c.start_line,
                    c.end_line,
                )
            })
            .collect();
        // Task 85 added module-doc capture: the leading `//!` comment on
        // line 1 now becomes the first chunk before all item chunks.
        // end_line is 2 because tree-sitter's line_comment end_position row
        // includes the trailing newline (row 1, 0-indexed → line 2).
        let expected = vec![
            ("module-doc", None, 1, 2),
            ("struct", Some("Point"), 5, 10),
            ("enum", Some("Dir"), 12, 16),
            ("trait", Some("Greet"), 18, 21),
            ("type", Some("Coord"), 23, 23),
            ("const", Some("MAX"), 25, 25),
            ("static", Some("NAME"), 27, 27),
            ("macro", Some("shout"), 29, 31),
            ("function", Some("add"), 33, 37),
            ("method", Some("Point::new"), 40, 43),
            ("method", Some("Point::magnitude"), 45, 47),
            ("function", Some("helper"), 51, 51),
            ("struct", Some("Cfg"), 53, 53),
        ];
        assert_eq!(got, expected);
    }

    fn symbols(chunks: &[Chunk]) -> Vec<(&str, Option<&str>)> {
        chunks
            .iter()
            .map(|c| (c.kind.as_str(), c.symbol.as_deref()))
            .collect()
    }

    const CS_FIXTURE: &str = r#"namespace App;

/// <summary>A widget.</summary>
public class Widget
{
    private int _count;

    public Widget(int count) { _count = count; }

    public void Increment() { _count++; }

    private class Nested { public void Deep() {} }
}
"#;

    const JAVA_FIXTURE: &str = r#"package app;

/** A widget. */
public class Widget {
    private int count;

    public Widget(int count) { this.count = count; }

    public int getCount() { return count; }
}
"#;

    #[test]
    fn csharp_class_emits_header_and_qualified_member_chunks() {
        let chunks = chunk_file("Widget.cs", CS_FIXTURE, Some("csharp"));
        assert_eq!(
            symbols(&chunks),
            vec![
                ("class", Some("Widget")),
                ("constructor", Some("Widget::Widget")),
                ("method", Some("Widget::Increment")),
                ("class", Some("Widget::Nested")),
                ("method", Some("Widget::Nested::Deep")),
            ]
        );
    }

    #[test]
    fn csharp_class_header_is_signature_only() {
        let chunks = chunk_file("Widget.cs", CS_FIXTURE, Some("csharp"));
        let header = chunks
            .iter()
            .find(|c| c.kind == "class")
            .expect("class chunk");
        // Folded class doc and non-method members are kept verbatim.
        assert!(header.body.contains("A widget."), "class doc folded in");
        assert!(header.body.contains("private int _count;"), "field kept");
        // Method / constructor / nested-type bodies are excised.
        assert!(header.body.contains("{ ... }"), "bodies excised");
        assert!(!header.body.contains("_count++"), "method body excised");
        assert!(!header.body.contains("_count = count"), "ctor body excised");
        assert!(
            !header.body.contains("Deep() {}"),
            "nested-type body excised"
        );
    }

    #[test]
    fn java_class_emits_header_and_qualified_member_chunks() {
        let chunks = chunk_file("Widget.java", JAVA_FIXTURE, Some("java"));
        assert_eq!(
            symbols(&chunks),
            vec![
                ("class", Some("Widget")),
                ("constructor", Some("Widget::Widget")),
                ("method", Some("Widget::getCount")),
            ]
        );
    }

    #[test]
    fn java_class_header_folds_javadoc_and_excises_bodies() {
        let chunks = chunk_file("Widget.java", JAVA_FIXTURE, Some("java"));
        let header = chunks
            .iter()
            .find(|c| c.kind == "class")
            .expect("class chunk");
        assert!(header.body.contains("A widget."), "javadoc folded in");
        assert!(header.body.contains("private int count;"), "field kept");
        assert!(header.body.contains("{ ... }"), "bodies excised");
        assert!(!header.body.contains("return count"), "method body excised");
        assert!(
            !header.body.contains("this.count = count"),
            "ctor body excised"
        );
    }

    // M11 review M2: multiple methods in a single class produce multiple
    // excision ranges; verify each body is replaced and no panic occurs.
    #[test]
    fn csharp_class_header_excises_all_member_bodies() {
        let src =
            "class Foo {\n  void A() { doA(); }\n  void B() { doB(); }\n  void C() { doC(); }\n}\n";
        let chunks = chunk_file("Foo.cs", src, Some("csharp"));
        let header = chunks
            .iter()
            .find(|c| c.kind == "class")
            .expect("class chunk");
        // None of the concrete body statements should survive into the header.
        assert!(!header.body.contains("doA()"), "A body must be excised");
        assert!(!header.body.contains("doB()"), "B body must be excised");
        assert!(!header.body.contains("doC()"), "C body must be excised");
        // Each body is replaced with the placeholder.
        assert_eq!(
            header.body.matches("{ ... }").count(),
            3,
            "expected three placeholder regions, got: {}",
            header.body
        );
    }

    // M11 review L2: body-less members (interface methods, abstract methods,
    // C# auto-properties) intentionally appear in BOTH the type header chunk
    // AND as their own individual member chunk. This test pins that behavior
    // so it is not accidentally changed.
    #[test]
    fn bodyless_members_appear_in_header_and_as_own_chunk() {
        // C# interface: void Method() has no body block.
        let src = "interface IFoo { void Method(); }\n";
        let chunks = chunk_file("IFoo.cs", src, Some("csharp"));
        // The interface header chunk must contain the method signature verbatim.
        let header = chunks
            .iter()
            .find(|c| c.kind == "interface")
            .expect("interface chunk");
        assert!(
            header.body.contains("void Method()"),
            "body-less method must stay verbatim in header; got: {}",
            header.body
        );
        // The method must also be emitted as its own chunk.
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol.as_deref() == Some("IFoo::Method")),
            "body-less method must still get its own member chunk; chunks: {chunks:?}"
        );
        // No placeholder was inserted (there was nothing to excise).
        assert!(
            !header.body.contains("{ ... }"),
            "body-less member must not produce a placeholder"
        );
    }

    fn js(content: &str) -> Vec<Chunk> {
        chunk_file("src/app.js", content, Some("javascript"))
    }

    fn ts(content: &str) -> Vec<Chunk> {
        chunk_file("src/app.ts", content, Some("typescript"))
    }

    // The dominant JS/TS case: a top-level `const f = () => {}` takes its
    // name from the binding declarator (the JsDeclarator hook), and a plain
    // data binding produces no symbol chunk.
    #[test]
    fn js_arrow_binding_is_named_from_its_declarator() {
        let chunks = js("const add = (a, b) => a + b;\nconst answer = 42;\n");
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("add"))), "{got:?}");
        // `const answer = 42` is data, not a function — no symbol chunk.
        assert!(!got.iter().any(|(_, s)| *s == Some("answer")), "{got:?}");
    }

    #[test]
    fn js_function_declaration_and_class_methods_are_chunked() {
        let src = "\
function div(a, b) { return a / b; }
class Counter {
  inc() { this.n++; }
  onClick = () => { this.n = 0; };
}
";
        let chunks = js(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("div"))), "{got:?}");
        assert!(got.contains(&("class", Some("Counter"))), "{got:?}");
        assert!(got.contains(&("method", Some("Counter::inc"))), "{got:?}");
        // A class-field arrow is a qualified method, too.
        assert!(
            got.contains(&("method", Some("Counter::onClick"))),
            "{got:?}"
        );
    }

    // `export` wraps nearly every top-level declaration; the walker must see
    // through `export_statement` or it indexes almost nothing in real code.
    #[test]
    fn exported_declarations_are_seen_through_the_export_wrapper() {
        let src = "\
export function alpha() {}
export const beta = () => {};
export class Gamma { m() {} }
";
        let chunks = js(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("alpha"))), "{got:?}");
        assert!(got.contains(&("function", Some("beta"))), "{got:?}");
        assert!(got.contains(&("class", Some("Gamma"))), "{got:?}");
        assert!(got.contains(&("method", Some("Gamma::m"))), "{got:?}");
    }

    // A JSDoc comment sits above the `export` wrapper, not the wrapped
    // declaration; folding must climb through the wrapper to reach it.
    #[test]
    fn jsdoc_above_an_exported_function_folds_into_the_chunk() {
        let src = "\
/** Adds two numbers. */
export function add(a, b) { return a + b; }
";
        let chunks = js(src);
        let c = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("add"))
            .expect("add chunk");
        assert_eq!(c.start_line, 1, "chunk should start at the JSDoc line");
        assert!(c.body.contains("Adds two numbers."), "JSDoc folded in");
        assert!(
            c.body.contains("export function add"),
            "export kept in body"
        );
    }

    // In a multi-binding statement only the function declarator is chunked,
    // and on its own so the data sibling is not swept into its body.
    #[test]
    fn multi_binding_statement_chunks_only_the_function_declarator() {
        let chunks = js("let a = 1, sq = (x) => x * x;\n");
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("sq"))), "{got:?}");
        assert!(!got.iter().any(|(_, s)| *s == Some("a")), "{got:?}");
        let sq = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("sq"))
            .expect("sq chunk");
        assert!(!sq.body.contains("a = 1"), "data sibling must not leak in");
    }

    #[test]
    fn ts_interface_type_alias_and_enum_are_chunked() {
        let src = "\
export interface Repo {
  find(id: string): Item;
  size: number;
}
export type Id = string | number;
export enum Color { Red, Green }
";
        let chunks = ts(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("interface", Some("Repo"))), "{got:?}");
        // Interface methods are body-less members, addressable on their own.
        assert!(got.contains(&("method", Some("Repo::find"))), "{got:?}");
        assert!(got.contains(&("type_alias", Some("Id"))), "{got:?}");
        assert!(got.contains(&("enum", Some("Color"))), "{got:?}");
    }

    #[test]
    fn ts_abstract_class_and_arrow_field_method_are_chunked() {
        let src = "\
export abstract class Widget {
  private count = 0;
  constructor(c: number) { this.count = c; }
  increment(): void { this.count++; }
  handle = () => { this.count = 0; };
}
";
        let chunks = ts(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("abstract_class", Some("Widget"))), "{got:?}");
        assert!(
            got.contains(&("method", Some("Widget::constructor"))),
            "{got:?}"
        );
        assert!(
            got.contains(&("method", Some("Widget::increment"))),
            "{got:?}"
        );
        assert!(got.contains(&("method", Some("Widget::handle"))), "{got:?}");
    }

    #[test]
    fn ts_namespace_members_are_recursed_into() {
        // A namespace parses as `expression_statement` > `internal_module`;
        // both wrappers must be walked through to reach the members.
        let src = "\
namespace Geo {
  export const dist = (a: number, b: number) => b - a;
  export class Point { x(): number { return 0; } }
}
";
        let chunks = ts(src);
        let got = symbols(&chunks);
        // Namespaces do not qualify member names (matching C#/Rust modules).
        assert!(got.contains(&("function", Some("dist"))), "{got:?}");
        assert!(got.contains(&("class", Some("Point"))), "{got:?}");
        assert!(got.contains(&("method", Some("Point::x"))), "{got:?}");
    }

    #[test]
    fn ts_class_header_excises_method_bodies() {
        let src = "\
export class Service {
  private url = \"/api\";
  fetch(): void { doFetch(this.url); }
}
";
        let chunks = ts(src);
        let header = chunks
            .iter()
            .find(|c| c.kind == "class")
            .expect("class chunk");
        assert!(header.body.contains("private url"), "field kept in header");
        assert!(header.body.contains("{ ... }"), "method body excised");
        assert!(
            !header.body.contains("doFetch"),
            "method body must be excised"
        );
    }

    fn py(content: &str) -> Vec<Chunk> {
        chunk_file("src/app.py", content, Some("python"))
    }

    // Module-level `def` is a function; a `class` emits a header chunk plus a
    // `Class::method` chunk per method (including `async def`).
    #[test]
    fn python_module_function_and_class_methods_are_chunked() {
        let src = "\
def top_level(a, b):
    return a + b


class Widget:
    def __init__(self, n):
        self.n = n

    async def fetch(self):
        return await thing()
";
        let chunks = py(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("top_level"))), "{got:?}");
        assert!(got.contains(&("class", Some("Widget"))), "{got:?}");
        assert!(
            got.contains(&("method", Some("Widget::__init__"))),
            "{got:?}"
        );
        // `async def` is still a `function_definition`.
        assert!(got.contains(&("method", Some("Widget::fetch"))), "{got:?}");
    }

    // The PythonDocstring hook climbs the `decorated_definition` wrapper so a
    // def's decorators are folded into its chunk (and it is still named from
    // the def, not skipped).
    #[test]
    fn python_decorators_fold_into_the_function_chunk() {
        let src = "\
@app.route(\"/x\")
@cached
def handler(req):
    return 200
";
        let chunks = py(src);
        let c = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("handler"))
            .expect("handler chunk");
        assert_eq!(c.start_line, 1, "chunk should start at the first decorator");
        assert!(
            c.body.contains("@app.route"),
            "decorator folded in: {}",
            c.body
        );
        assert!(c.body.contains("@cached"), "second decorator folded in");
        assert!(c.body.contains("def handler"), "def kept in body");
    }

    // Unlike PrecedingSiblings, a `#` comment above a def is NOT folded —
    // Python hash comments are not documentation.
    #[test]
    fn python_hash_comments_are_not_folded_as_docs() {
        let src = "\
# a leading hash comment
def solo():
    return 1
";
        let chunks = py(src);
        let c = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("solo"))
            .expect("solo chunk");
        assert_eq!(c.start_line, 2, "chunk starts at the def, not the comment");
        assert!(!c.body.contains("hash comment"), "comment must not fold in");
    }

    // A class's leading docstring stays in the header chunk (it is the first
    // body `expression_statement`, which has no body to excise), while every
    // method body — plain, decorated, or async — is excised to `{ ... }`.
    #[test]
    fn python_class_header_keeps_docstring_and_excises_method_bodies() {
        let src = "\
class Widget:
    \"\"\"A widget.\"\"\"

    count = 0

    def __init__(self, n):
        self.n = n

    @property
    def doubled(self):
        return self.n * 2
";
        let chunks = py(src);
        let header = chunks
            .iter()
            .find(|c| c.kind == "class")
            .expect("class chunk");
        assert!(
            header.body.contains("A widget."),
            "docstring kept in header"
        );
        assert!(header.body.contains("count = 0"), "class field kept");
        assert!(
            header.body.contains("@property"),
            "decorator kept in header"
        );
        assert!(!header.body.contains("self.n = n"), "ctor body excised");
        assert!(
            !header.body.contains("self.n * 2"),
            "decorated method body excised"
        );
        assert!(
            header.body.contains("{ ... }"),
            "bodies excised to placeholder"
        );
        // The decorated method is still addressable as its own chunk.
        let got = symbols(&chunks);
        assert!(
            got.contains(&("method", Some("Widget::doubled"))),
            "{got:?}"
        );
    }

    // A decorated class at module level: the walker recurses through the
    // wrapper to the class, and the class decorators fold into the header.
    #[test]
    fn python_decorated_class_is_chunked_with_its_decorator() {
        let src = "\
@dataclass
class Point:
    x: int
    y: int
";
        let chunks = py(src);
        let header = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("Point"))
            .expect("Point chunk");
        assert_eq!(header.kind, "class");
        assert_eq!(header.start_line, 1, "chunk starts at the @dataclass line");
        assert!(
            header.body.contains("@dataclass"),
            "class decorator folded in"
        );
    }

    fn go(content: &str) -> Vec<Chunk> {
        chunk_file("main.go", content, Some("go"))
    }

    // Free functions are `function`; a `method_declaration` is `method`,
    // qualified by its receiver type — pointer (`*Point`) and value (`Point`)
    // receivers both strip to the bare type name.
    #[test]
    fn go_free_function_and_methods_are_chunked() {
        let src = "\
package geo

func Dist(a, b int) int { return b - a }

type Point struct{ X, Y int }

func (p *Point) Mag() float64 { return 0.0 }

func (p Point) Name() string { return \"p\" }
";
        let chunks = go(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("Dist"))), "{got:?}");
        assert!(got.contains(&("method", Some("Point::Mag"))), "{got:?}");
        assert!(got.contains(&("method", Some("Point::Name"))), "{got:?}");
    }

    // A generic receiver (`*Stack[T]`) strips both the pointer and the type
    // parameters down to the base type name.
    #[test]
    fn go_generic_receiver_strips_to_base_type() {
        let src = "\
package stack

type Stack[T any] struct{ items []T }

func (s *Stack[T]) Push(v T) { s.items = append(s.items, v) }
";
        let chunks = go(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("method", Some("Stack::Push"))), "{got:?}");
    }

    // Structs and interfaces are leaf `type` chunks — Go methods live outside
    // the type, so there is no header/member split. (The `=` alias form is a
    // distinct `type_alias` node, out of scope: such a file line-windows.)
    #[test]
    fn go_struct_and_interface_are_leaf_type_chunks() {
        let src = "\
package model

type Widget struct {
	count int
}

type Shape interface {
	Area() float64
}
";
        let chunks = go(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("type", Some("Widget"))), "{got:?}");
        assert!(got.contains(&("type", Some("Shape"))), "{got:?}");
        // A struct is a single leaf chunk: its field body is kept, not excised.
        let widget = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("Widget"))
            .expect("Widget chunk");
        assert!(
            widget.body.contains("count int"),
            "struct body kept verbatim"
        );
        assert!(
            !widget.body.contains("{ ... }"),
            "no excision for a leaf type"
        );
    }

    // Package-level const/var, including grouped `( … )` forms — grouped `var`
    // nests its specs under a `var_spec_list` the walker must see through.
    #[test]
    fn go_grouped_const_and_var_specs_are_chunked() {
        let src = "\
package cfg

const Pi = 3.14

const (
	A = 1
	B = 2
)

var lonely int

var (
	x = 1
	y = 2
)
";
        let chunks = go(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("const", Some("Pi"))), "{got:?}");
        assert!(got.contains(&("const", Some("A"))), "{got:?}");
        assert!(got.contains(&("const", Some("B"))), "{got:?}");
        assert!(got.contains(&("var", Some("lonely"))), "{got:?}");
        assert!(got.contains(&("var", Some("x"))), "{got:?}");
        assert!(got.contains(&("var", Some("y"))), "{got:?}");
    }

    // A godoc `//` comment immediately above a declaration folds into its chunk.
    #[test]
    fn go_doc_comment_folds_into_chunk() {
        let src = "\
package geo

// Dist returns the distance between a and b.
func Dist(a, b int) int { return b - a }
";
        let chunks = go(src);
        let c = chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("Dist"))
            .expect("Dist chunk");
        assert_eq!(c.start_line, 3, "chunk should start at the godoc line");
        assert!(
            c.body.contains("Dist returns the distance"),
            "godoc folded in"
        );
        assert!(c.body.contains("func Dist"), "func kept in body");
    }

    #[test]
    fn empty_file_yields_no_chunks() {
        assert!(chunk_file("src/empty.rs", "", Some("rust")).is_empty());
        assert!(chunk_file("src/empty.rs", "   \n\n  ", None).is_empty());
    }

    #[test]
    fn free_function_becomes_one_function_chunk() {
        let chunks = rust("fn main() {\n    println!(\"hi\");\n}\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, "function");
        assert_eq!(chunks[0].symbol.as_deref(), Some("main"));
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert!(
            chunks[0]
                .embed_text
                .starts_with("src/lib.rs\nfunction main\n\n")
        );
    }

    #[test]
    fn struct_enum_trait_each_get_a_kind_tagged_chunk() {
        let src = "\
struct Point { x: i32, y: i32 }
enum Dir { N, S }
trait Greet { fn hi(&self); }
";
        let chunks = rust(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("struct", Some("Point"))), "{got:?}");
        assert!(got.contains(&("enum", Some("Dir"))), "{got:?}");
        assert!(got.contains(&("trait", Some("Greet"))), "{got:?}");
    }

    #[test]
    fn impl_methods_are_chunked_as_type_qualified_methods() {
        let src = "\
struct Counter { n: u32 }
impl Counter {
    fn new() -> Self { Counter { n: 0 } }
    fn inc(&mut self) { self.n += 1; }
}
";
        let chunks = rust(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("struct", Some("Counter"))), "{got:?}");
        assert!(got.contains(&("method", Some("Counter::new"))), "{got:?}");
        assert!(got.contains(&("method", Some("Counter::inc"))), "{got:?}");
        // The impl block itself is not emitted as a chunk.
        assert!(!got.iter().any(|(k, _)| *k == "impl"), "{got:?}");
    }

    #[test]
    fn preceding_doc_comments_and_attributes_fold_into_the_chunk() {
        let src = "\
/// Adds two numbers.
/// Second line of docs.
#[inline]
fn add(a: i32, b: i32) -> i32 { a + b }
";
        let chunks = rust(src);
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        assert_eq!(c.symbol.as_deref(), Some("add"));
        // The chunk starts at the first doc-comment line, not the `fn`.
        assert_eq!(c.start_line, 1);
        assert!(c.body.contains("Adds two numbers."), "doc folded in");
        assert!(c.body.contains("#[inline]"), "attribute folded in");
        assert!(c.body.contains("fn add"));
    }

    #[test]
    fn a_blank_line_stops_doc_comment_folding() {
        let src = "\
// Unrelated header comment.

fn solo() {}
";
        let chunks = rust(src);
        assert_eq!(chunks.len(), 1);
        // The blank line between the comment and `fn` breaks the run.
        assert_eq!(chunks[0].start_line, 3);
        assert!(!chunks[0].body.contains("Unrelated header"));
    }

    #[test]
    fn inline_module_items_are_recursed_into() {
        let src = "\
mod inner {
    fn helper() {}
    struct Cfg;
}
";
        let chunks = rust(src);
        let got = symbols(&chunks);
        assert!(got.contains(&("function", Some("helper"))), "{got:?}");
        assert!(got.contains(&("struct", Some("Cfg"))), "{got:?}");
    }

    #[test]
    fn unparseable_rust_falls_back_to_line_windows() {
        // No recognizable items: a fragment of only imports. The AST
        // chunker yields nothing, so line windows cover the bytes.
        let src = "use std::fs;\nuse std::io;\n";
        let chunks = rust(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, LINE_WINDOW_KIND);
        assert_eq!(chunks[0].symbol, None);
    }

    #[test]
    fn no_grammar_language_uses_line_windows() {
        let chunks = chunk_file("README.md", "# Title\n\nbody text\n", Some("markdown"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, LINE_WINDOW_KIND);
        assert!(chunks[0].embed_text.starts_with("README.md\n\n"));
    }

    #[test]
    fn crlf_body_is_normalized_to_lf() {
        let chunks = rust("fn a() {\r\n    let x = 1;\r\n}\r\n");
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].body.contains('\r'), "CRLF must be normalized");
    }

    #[test]
    fn long_single_line_is_split_under_the_byte_cap() {
        let content = "x".repeat(MAX_CHUNK_BYTES * 3 + 17);
        let chunks = chunk_line_windows("dist/app.min.js", &content);
        assert!(chunks.len() >= 4, "expected the oversized line to split");
        for c in &chunks {
            assert!(c.body.len() <= MAX_CHUNK_BYTES, "chunk over the byte cap");
            assert_eq!(c.start_line, 1);
            assert_eq!(c.end_line, 1);
        }
        let joined: String = chunks.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(joined, content);
    }

    #[test]
    fn windows_are_contiguous_and_cover_every_line() {
        let content = (1..=125)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_line_windows("src/big.rs", &content);
        assert_eq!(chunks.len(), 3); // 50 + 50 + 25
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 50);
        assert_eq!(chunks[2].start_line, 101);
        assert_eq!(chunks[2].end_line, 125);
    }

    // Task 87: per-language module-doc capture. For the comment-marker
    // languages (C#/Java/TS/JS) the gap gate means a DETACHED doc (preamble or
    // blank-line gap) is a file doc, while one ADJACENT to a declaration is that
    // declaration's doc and must NOT be promoted to a standalone module-doc.

    #[test]
    fn csharp_detached_xml_doc_becomes_module_doc() {
        // `/// ` followed by a preamble (using directive), not a declaration.
        let src = "/// <summary>File-level doc.</summary>\nusing System;\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.cs", src, Some("csharp"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("File-level doc."),
            "module-doc body: {}",
            doc.unwrap().body
        );
        assert_eq!(doc.unwrap().start_line, 1, "module-doc starts at line 1");
    }

    #[test]
    fn csharp_xml_doc_adjacent_to_class_is_not_module_doc() {
        // Adjacent to a class_declaration → that class's doc (gap gate), not a file doc.
        let src = "/// <summary>Class doc.</summary>\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.cs", src, Some("csharp"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "adjacent /// must fold into the class, not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn csharp_plain_comment_does_not_become_module_doc() {
        let src = "// plain comment\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.cs", src, Some("csharp"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "plain // comment must not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn java_detached_javadoc_becomes_module_doc() {
        // `/** */` before the `package` preamble → file doc.
        let src = "/** File-level Javadoc. */\npackage app;\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.java", src, Some("java"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("File-level Javadoc."),
            "module-doc body: {}",
            doc.unwrap().body
        );
    }

    #[test]
    fn java_javadoc_adjacent_to_class_is_not_module_doc() {
        let src = "/** Class doc. */\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.java", src, Some("java"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "adjacent /** must fold into the class, not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn java_plain_comment_does_not_become_module_doc() {
        let src = "// plain comment\npackage app;\npublic class Foo {}\n";
        let chunks = chunk_file("Foo.java", src, Some("java"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "plain // comment must not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn ts_detached_jsdoc_becomes_module_doc() {
        // A blank line separates the file JSDoc from the first export.
        let src = "/** @module geo - 2D geometry. */\n\nexport function dist(a: number, b: number): number { return b - a; }\n";
        let chunks = chunk_file("geo.ts", src, Some("typescript"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("2D geometry."),
            "module-doc body: {}",
            doc.unwrap().body
        );
    }

    #[test]
    fn ts_jsdoc_adjacent_to_function_is_not_module_doc() {
        // No blank line → it is the function's JSDoc (wrapped in export_statement,
        // a transparent decl kind), so the gap gate suppresses a module-doc.
        let src = "/** Returns the distance. */\nexport function dist(a: number, b: number): number { return b - a; }\n";
        let chunks = chunk_file("geo.ts", src, Some("typescript"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "adjacent /** must not be promoted to module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn ts_plain_comment_does_not_become_module_doc() {
        let src = "// plain comment\nexport function dist(a: number, b: number): number { return b - a; }\n";
        let chunks = chunk_file("geo.ts", src, Some("typescript"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "plain // comment must not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn js_detached_jsdoc_becomes_module_doc() {
        let src = "/** @module utils - Utility functions. */\n\nexport function add(a, b) { return a + b; }\n";
        let chunks = chunk_file("utils.js", src, Some("javascript"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("Utility functions."),
            "module-doc body: {}",
            doc.unwrap().body
        );
    }

    #[test]
    fn js_jsdoc_adjacent_to_function_is_not_module_doc() {
        let src = "/** Adds two numbers. */\nexport function add(a, b) { return a + b; }\n";
        let chunks = chunk_file("utils.js", src, Some("javascript"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "adjacent /** must not be promoted to module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn js_plain_comment_does_not_become_module_doc() {
        let src = "// plain comment\nexport function add(a, b) { return a + b; }\n";
        let chunks = chunk_file("utils.js", src, Some("javascript"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "plain // comment must not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn python_module_docstring_becomes_module_doc() {
        let src = "\"\"\"Module-level docstring.\"\"\"\n\ndef foo():\n    pass\n";
        let chunks = chunk_file("app.py", src, Some("python"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("Module-level docstring."),
            "module-doc body: {}",
            doc.unwrap().body
        );
    }

    #[test]
    fn python_hash_comment_does_not_become_module_doc() {
        let src = "# plain hash comment\n\ndef foo():\n    pass\n";
        let chunks = chunk_file("app.py", src, Some("python"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "hash comment must not emit module-doc; got: {chunks:?}"
        );
    }

    #[test]
    fn go_package_doc_comment_becomes_module_doc() {
        let src = "// Package geo provides 2D geometry.\npackage geo\n\nfunc Dist(a, b int) int { return b - a }\n";
        let chunks = chunk_file("geo.go", src, Some("go"));
        let doc = chunks.iter().find(|c| c.kind == "module-doc");
        assert!(
            doc.is_some(),
            "expected a module-doc chunk; got: {chunks:?}"
        );
        assert!(
            doc.unwrap().body.contains("2D geometry."),
            "module-doc body: {}",
            doc.unwrap().body
        );
        assert_eq!(doc.unwrap().start_line, 1, "module-doc starts at line 1");
    }

    #[test]
    fn go_no_package_doc_no_module_doc() {
        let src = "package geo\n\nfunc Dist(a, b int) int { return b - a }\n";
        let chunks = chunk_file("geo.go", src, Some("go"));
        assert!(
            !chunks.iter().any(|c| c.kind == "module-doc"),
            "no preceding comment means no module-doc; got: {chunks:?}"
        );
    }
}

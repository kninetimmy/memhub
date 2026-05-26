//! Code chunker (M11, decision 107).
//!
//! Two strategies behind one [`chunk_file`] entry point:
//!
//! * A **tree-sitter AST chunker** ([`chunk_with_grammar`]) for languages
//!   with a registered grammar ([`super::grammar`]). It emits one chunk
//!   per top-level item — functions, structs, enums, traits, type/const/
//!   static items, macros — and one chunk per `impl` method (named
//!   `Type::method`), each carrying its symbol name and a kind tag. A
//!   contiguous run of preceding doc comments / attributes is folded into
//!   the item's chunk so the model sees the documentation with the code.
//!
//! * A **line-window fallback** ([`chunk_line_windows`]) — fixed-size,
//!   symbol-unaware windows — used when the language has no grammar, when
//!   parsing yields no items, or for the bytes of an otherwise unhandled
//!   file. Nothing is ever silently dropped: every non-empty tracked text
//!   file produces at least one chunk.

use tree_sitter::{Node, Parser};

use super::grammar::{self, GrammarSpec};

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
        let chunks = chunk_with_grammar(path, content, &spec);
        // A parse that surfaces no items (e.g. a file of only `use`
        // statements, or one too broken to recover symbols) falls through
        // to line windows so its bytes stay searchable — never dropped.
        if !chunks.is_empty() {
            return chunks;
        }
    }
    chunk_line_windows(path, content)
}

/// Walk the AST and emit one chunk per top-level item / impl method.
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
    collect_items(tree.root_node(), src, path, spec, None, &mut chunks);
    chunks
}

/// Recursively collect item chunks from `node`'s named children.
/// `type_prefix` is set while inside an `impl` block so methods are named
/// `Type::method`.
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

        if kind == spec.function_kind {
            let name = field_text(&child, "name", src);
            let symbol = match (type_prefix, name) {
                (Some(ty), Some(n)) => Some(format!("{ty}::{n}")),
                (None, Some(n)) => Some(n.to_string()),
                _ => None,
            };
            let label = if type_prefix.is_some() {
                "method"
            } else {
                "function"
            };
            push_symbol(out, path, src, spec, child, symbol, label);
        } else if kind == spec.impl_kind {
            // Emit the methods, not the whole (possibly huge) impl block.
            let prefix = field_text(&child, "type", src);
            if let Some(body) = child.child_by_field_name("body") {
                collect_items(body, src, path, spec, prefix, out);
            }
        } else if kind == spec.module_kind {
            // Recurse into an inline `mod foo { … }`; an out-of-line
            // `mod foo;` has no body here and is indexed via its own file.
            if let Some(body) = child.child_by_field_name("body") {
                collect_items(body, src, path, spec, type_prefix, out);
            }
        } else if spec.item_kinds.contains(&kind) {
            let symbol = field_text(&child, "name", src).map(str::to_string);
            push_symbol(out, path, src, spec, child, symbol, kind_label(kind));
        }
    }
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

/// Walk backwards from `item` over contiguous preceding doc comments and
/// attributes, returning the extended `(start_byte, start_row)`. A blank
/// line (a row gap > 1) between siblings stops the run, so an unrelated
/// license header far above is not absorbed.
fn leading_start(item: Node<'_>, spec: &GrammarSpec) -> (usize, usize) {
    let mut earliest = item;
    let mut sib = item.prev_sibling();
    while let Some(s) = sib {
        let k = s.kind();
        let foldable =
            k == spec.line_comment_kind || k == spec.block_comment_kind || k == spec.attribute_kind;
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
/// `_definition` so `struct_item` → `struct`, `macro_definition` → `macro`.
fn kind_label(node_kind: &str) -> &str {
    node_kind
        .strip_suffix("_item")
        .or_else(|| node_kind.strip_suffix("_definition"))
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

    fn symbols(chunks: &[Chunk]) -> Vec<(&str, Option<&str>)> {
        chunks
            .iter()
            .map(|c| (c.kind.as_str(), c.symbol.as_deref()))
            .collect()
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
}

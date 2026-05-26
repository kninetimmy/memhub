//! Line-window placeholder chunker (M11 PR1, decision 107).
//!
//! A deliberately dumb chunker: fixed-size, non-overlapping line windows
//! with no symbol awareness. It exists so the spine — walker, staleness
//! diff, persistence — has real chunk rows to operate on. PR2 replaces it
//! wholesale with a tree-sitter AST chunker (one chunk per item, with the
//! symbol name and kind captured); files that fail to parse there fall
//! back to exactly this windowing so nothing is ever silently dropped.

/// Lines per window. A placeholder value — tuned for retrieval in PR2/PR5.
pub const WINDOW_LINES: usize = 50;

/// Kind tag stored on every line-window chunk. Distinguishes placeholder
/// chunks from the symbol-aware chunks PR2 will emit.
pub const LINE_WINDOW_KIND: &str = "line-window";

/// One chunk produced from a source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// 1-indexed inclusive start line.
    pub start_line: usize,
    /// 1-indexed inclusive end line.
    pub end_line: usize,
    /// Symbol name, when the chunker knows one. Always `None` for
    /// line-window chunks; PR2's AST chunker fills it.
    pub symbol: Option<String>,
    /// Chunk kind tag (e.g. `line-window`, later `function`, `struct`).
    pub kind: String,
    /// Text fed to the embedder / FTS. For PR1: `path` + the window body,
    /// so retrieval has the file location as signal. PR2 extends this to
    /// `path + kind + name + body`.
    pub embed_text: String,
    /// The raw window body (without the path prefix), used to hash the
    /// chunk so a content-stable window doesn't churn.
    pub body: String,
}

/// Split `content` into fixed line windows. An empty (or whitespace-only)
/// file yields no chunks. Line numbers are 1-indexed and inclusive.
pub fn chunk_file(path: &str, content: &str) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + WINDOW_LINES).min(lines.len());
        let body = lines[start..end].join("\n");
        let embed_text = format!("{path}\n\n{body}");
        chunks.push(Chunk {
            start_line: start + 1,
            end_line: end,
            symbol: None,
            kind: LINE_WINDOW_KIND.to_string(),
            embed_text,
            body,
        });
        start = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_yields_no_chunks() {
        assert!(chunk_file("src/empty.rs", "").is_empty());
        assert!(chunk_file("src/empty.rs", "   \n\n  ").is_empty());
    }

    #[test]
    fn short_file_is_one_chunk_covering_all_lines() {
        let content = "fn main() {\n    println!(\"hi\");\n}";
        let chunks = chunk_file("src/main.rs", content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[0].kind, LINE_WINDOW_KIND);
        assert_eq!(chunks[0].symbol, None);
        assert!(chunks[0].embed_text.starts_with("src/main.rs\n\n"));
    }

    #[test]
    fn windows_are_contiguous_and_cover_every_line() {
        let content = (1..=125)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_file("src/big.rs", &content);
        assert_eq!(chunks.len(), 3); // 50 + 50 + 25
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 50);
        assert_eq!(chunks[1].start_line, 51);
        assert_eq!(chunks[1].end_line, 100);
        assert_eq!(chunks[2].start_line, 101);
        assert_eq!(chunks[2].end_line, 125);
    }
}

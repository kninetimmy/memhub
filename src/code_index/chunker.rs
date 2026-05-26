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

/// Hard byte cap on a single chunk body. A window is flushed early once
/// adding the next line would exceed this, and a single line longer than
/// the cap is split on UTF-8 char boundaries into multiple chunks. Without
/// this, a minified or generated single-line file becomes one unbounded
/// chunk that PR2's embedder (a fixed context window) cannot ingest.
pub const MAX_CHUNK_BYTES: usize = 4000;

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
    /// The window body (without the path prefix). The caller hashes this
    /// to key PR2's per-chunk embedding cache, so a content-stable window
    /// doesn't re-embed. Note it is LF-normalized (built from
    /// [`str::lines`]), deliberately *unlike* the file-level `content_hash`
    /// over raw bytes: a CRLF↔LF re-checkout changes the file hash but must
    /// not churn chunk embeddings.
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
                push_chunk(&mut chunks, path, start + 1, start + 1, piece.to_string());
            }
        } else {
            push_chunk(&mut chunks, path, start + 1, end, body);
        }
        start = end;
    }
    chunks
}

fn push_chunk(
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
    fn long_single_line_is_split_under_the_byte_cap() {
        // A minified/generated file: one line far larger than the cap.
        let content = "x".repeat(MAX_CHUNK_BYTES * 3 + 17);
        let chunks = chunk_file("dist/app.min.js", &content);
        assert!(chunks.len() >= 4, "expected the oversized line to split");
        for c in &chunks {
            assert!(c.body.len() <= MAX_CHUNK_BYTES, "chunk over the byte cap");
            // Every piece points at the single source line.
            assert_eq!(c.start_line, 1);
            assert_eq!(c.end_line, 1);
        }
        // Reassembling the pieces recovers the original line.
        let joined: String = chunks.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(joined, content);
    }

    #[test]
    fn multibyte_split_never_breaks_a_char() {
        // '€' is 3 bytes; ensure splitting lands on char boundaries.
        let content = "€".repeat(MAX_CHUNK_BYTES); // ~3x the cap in bytes
        let chunks = chunk_file("src/unicode.rs", &content);
        for c in &chunks {
            assert!(c.body.len() <= MAX_CHUNK_BYTES);
            assert!(c.body.chars().all(|ch| ch == '€'), "char was split");
        }
        let joined: String = chunks.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(joined, content);
    }

    #[test]
    fn window_flushes_early_on_byte_budget() {
        // Many medium lines whose cumulative bytes exceed the cap well
        // before WINDOW_LINES lines accumulate.
        let line = "a".repeat(500);
        let content = std::iter::repeat_n(line.as_str(), 40)
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_file("src/wide.rs", &content);
        for c in &chunks {
            assert!(c.body.len() <= MAX_CHUNK_BYTES);
        }
        // 40 * 501 bytes / 4000 => several windows, not the single
        // 40-line window WINDOW_LINES alone would allow.
        assert!(chunks.len() > 1);
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

# Handoff: M11 chunker review fixes (C#/Java AST chunker)

**Audience:** a Sonnet session executing these fixes autonomously.
**Source of findings:** code review of commits `9368a77` (multi-language AST
chunker) and `786db91` (MCP instructions rewrite), reviewed at HEAD `786db91`.
**Status when written:** all findings are in *unpushed* commits on `main`. The
build compiles and `cargo test --lib code_index` is green (31 passed).

---

## Ground rules (read before touching anything)

1. **Verification command — must stay green after every change:**
   ```bash
   cargo test --lib code_index
   ```
2. **The Rust freeze test is sacred.** `src/code_index/chunker.rs::tests::rust_chunk_output_is_frozen`
   (~line 500) pins the exact Rust chunk output. None of these fixes should
   change Rust output. If a change *does* move those values, that is a red flag —
   stop and re-check; do **not** edit the expected list to make it pass unless
   you are certain the Rust behavior change is intended (it should not be for
   any fix here).
3. **Add a regression test for every behavioral fix in the same commit.**
4. Match surrounding code style; this file uses defensive `from_utf8(...).unwrap_or("")`
   slicing and heavy doc comments — follow suit.
5. Atomic commits: one logical fix per commit, imperative present-tense titles.
6. Do **not** push. Leave the commits local for the user to review.

---

## M1 (Medium, correctness) — Nested enums/delegates lose their type qualifier

**File:** `src/code_index/chunker.rs`, the `item_kinds` branch (~lines 147-149).

**Problem.** `collect_items` qualifies symbols with the enclosing `Type::` prefix
in the `type_container_kinds` branch and the `member_kinds` branch, but the
`item_kinds` branch ignores `type_prefix`:

```rust
} else if spec.item_kinds.contains(&kind) {
    let symbol = field_text(&child, "name", src).map(str::to_string); // NOT qualified
    push_symbol(out, path, src, spec, child, symbol, kind_label(kind));
}
```

C#'s `item_kinds` is `["enum_declaration", "delegate_declaration"]` and Java's is
`["enum_declaration", "annotation_type_declaration"]`. A nested type inside a
class therefore emits a **bare** symbol while a sibling nested *class* emits a
qualified one. Example:

```csharp
public class Widget {
    public enum Color { Red, Blue }   // emits ("enum", Some("Color"))  -- WRONG
    private class Nested { }          // emits ("class", Some("Widget::Nested")) -- correct
}
```

This breaks the qualified-breadcrumb scheme (decision 115) for a common, valid
shape of code.

**Fix.** Qualify in the `item_kinds` branch, matching the adjacent branches:

```rust
} else if spec.item_kinds.contains(&kind) {
    let symbol = qualify(type_prefix, field_text(&child, "name", src));
    push_symbol(out, path, src, spec, child, symbol, kind_label(kind));
}
```

**Why this is safe for Rust:** Rust's `item_kinds` are only ever reached with
`type_prefix == None` (top-level or inside a `mod`, which does not qualify), and
`qualify(None, Some(n)) == Some(n.to_string())`. So the freeze test stays
byte-identical.

**Test to add** (in `chunker.rs` tests module). Add a C# fixture with a
class-nested enum and assert the qualified name, e.g.:

```rust
const CS_NESTED_ENUM_FIXTURE: &str = r#"public class Widget {
    public enum Color { Red, Blue }
}
"#;

#[test]
fn csharp_nested_enum_is_type_qualified() {
    let chunks = chunk_file("Widget.cs", CS_NESTED_ENUM_FIXTURE, Some("csharp"));
    let got = symbols(&chunks);
    assert!(got.contains(&("enum", Some("Widget::Color"))), "{got:?}");
}
```

Verify the exact node-kind/name field by running the test; if `enum_declaration`
is nested differently than expected, adjust the assertion to the real output —
but the qualifier (`Widget::`) is the contract.

---

## M2 (Medium, robustness) — `slice_header` uses panic-on-bad-boundary slicing

**File:** `src/code_index/chunker.rs`, `slice_header` (~lines 266-296), specifically
the excise loop (~lines 280, 289-294).

**Problem.** Excise offsets are computed as `inner.start_byte() - start_byte`
(line ~280) — an unchecked `usize` subtraction — and then fed into **unchecked**
string indexing `&full[pos..s]` / `&full[pos..]` (lines ~289-294). Everywhere
else in this file, source slicing is guarded with
`std::str::from_utf8(&src[..]).unwrap_or("")` (see `push_symbol` ~line 215 and
`slice_header`'s own `full` at ~line 286). The invariant holds today because all
offsets come from one parse of one buffer, but a future doc-fold/attribute hook
that extends `start_byte` past a member would underflow and then panic on an OOB
index — taking down indexing for that file instead of degrading to the
line-window fallback (which the module contract at lines 14-17 promises).

**Fix.** Clamp the offsets and bounds-check the slices. Suggested shape:

```rust
// in the excise-collection loop:
let s = inner.start_byte().saturating_sub(start_byte);
let e = inner.end_byte().saturating_sub(start_byte);
excise.push((s, e));

// in the rebuild loop:
for (s, e) in excise {
    let s = s.min(full.len());
    let e = e.min(full.len());
    if pos <= s {
        header.push_str(&full[pos..s]);
    }
    header.push_str("{ ... }");
    pos = e.max(pos);
}
header.push_str(&full[pos.min(full.len())..]);
```

Keep behavior identical for the happy path (the existing C#/Java header tests
must still pass). This is hardening, not a behavior change.

**Test to add (optional but preferred):** none strictly required since behavior
is unchanged; the existing `csharp_class_header_is_signature_only` and
`java_class_header_folds_javadoc_and_excises_bodies` tests cover the happy path.
A unit test feeding deliberately out-of-range offsets isn't easily reachable
through the public API, so a code-level guard is the deliverable.

---

## L1 (Low, future-proofing) — `todo!()` hooks are runtime-panic paths

**File:** `src/code_index/chunker.rs`, `function_symbol` (~lines 185, 191) and
`leading_start` (~line 304): `todo!("JsDeclarator …")`, `todo!("GoReceiver …")`,
`todo!("PythonDocstring …")`.

**Problem.** These are unreachable from the registered grammars (Rust/C#/Java
all use `Direct`/`Standard`/`PrecedingSiblings`), so they cannot fire today.
But the moment a JS/TS/Python/Go `GrammarSpec` row is added in a later task,
indexing a file in that language will **panic** until the hook is implemented.

**Fix (lightweight — do not implement the hooks here, that is T3/T4/T5 scope).**
Leave the `todo!()`s but add a guard rail so a half-wired grammar can't ship
silently. Two acceptable options, pick one:
  - Add a unit test asserting every registered grammar uses only implemented
    hook variants (i.e., for each language `grammar_for` returns, assert
    `method_naming`/`function_naming`/`doc_fold` are in the implemented set).
    This fails CI the instant someone adds a JS row before wiring the hook.
  - Or add a brief `// SAFETY: unreachable until T3/T4/T5 — see <test>` comment
    cross-referencing the guard test.

Prefer the test — it is the real guard.

---

## L2 (Low, design call) — Header/member duplication for body-less members

**File:** `src/code_index/chunker.rs`, `slice_header` (~266-296) interacting with
the `member_kinds` branch (~150-153).

**Problem.** `slice_header` only excises a member's `body_field` subtree. Members
with **no** `body` field — interface method signatures, abstract methods, C#
auto-properties (which use an `accessors` field, not `body`) — are kept verbatim
in the type header **and** also emitted as their own `Type::member` chunk. For an
`interface_declaration` every method appears in full twice.

**RULING for this handoff (the user can override):** **Do the minimal, safe
thing — leave the current behavior as-is and only document it.** Suppressing the
duplicate member chunk risks dropping a legitimately useful standalone breadcrumb
for signature-only members, and the "right" answer depends on retrieval tuning
that is out of scope here. So for this task:
  - Add a short doc comment on `slice_header` (or `push_type_header`) noting that
    body-less members (interface methods, abstract methods, C# auto-properties)
    are intentionally retained in the header *and* emitted as member chunks,
    accepting the duplication until retrieval tuning (a later milestone) decides
    otherwise.
  - **Do not** change the chunking behavior.

If the user later wants dedup, that is a separate, tested change.

---

## L3 (Low, docs) — MCP instructions dropped still-registered tools

**File:** `src/mcp/mod.rs`, the `with_instructions(...)` string (~lines 745-765).

**Problem.** The rewrite (commit `786db91`) is a net improvement but no longer
mentions several still-registered tools: `status`, `search`, the `list_*` tools,
`get_command`, `render`, `sync_status`, and the write-only session-note surface.
The sync *verbs* are listed but `sync_status` is not; `render` (regenerate
PROJECT.md) has no pointer at all.

**Fix.** Add one concise line to the instructions string covering the
utility/read remainder so the routing table is not misread as exhaustive. Keep
the existing INTENT→TOOL / NEVER / OUT OF SCOPE structure; append something like:

```
OTHER (direct, use when explicitly needed): status, search, list_* (tasks/
decisions/facts/pending_writes), get_command, render (regenerate PROJECT.md),
sync_status, session-note (write-only scratch).
```

Match the exact tool names as registered in `src/mcp/mod.rs` — grep the tool
registrations to confirm spellings before writing them into the string. Do not
invent tool names.

---

## L4 (Low, docs) — Stale module/function doc comments

**File:** `src/code_index/chunker.rs`, module header (~lines 5-11) and
`chunk_with_grammar` doc (~lines 83-85).

**Problem.** The doc comments still describe the Rust-only model ("one chunk per
`impl` method (named `Type::method`)") and do not mention the new type-container
**header chunks** (`push_type_header`) or class members that C#/Java emit.

**Fix.** Update the prose to mention that type-container languages (C#/Java) emit
a header-only chunk per class/record/interface plus one `Type::member` chunk per
method/constructor/property. Keep it brief and accurate. No code change.

---

## Suggested commit sequence

1. `Qualify nested type items in C#/Java chunker (M11 review M1)` — code + test.
2. `Harden slice_header against out-of-range excise offsets (M11 review M2)` — code.
3. `Guard against unimplemented grammar hooks shipping (M11 review L1)` — test/comment.
4. `Document body-less member duplication in type headers (M11 review L2)` — doc only.
5. `Restore utility-tool pointers in MCP instructions (M11 review L3)` — string.
6. `Refresh chunker doc comments for type-container languages (M11 review L4)` — doc only.

Run `cargo test --lib code_index` after each. Optionally squash the doc-only
commits (3-6) if the user prefers fewer commits.

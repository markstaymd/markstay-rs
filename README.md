# markstay , Rust reference implementation (v1 core)

[![crates.io](https://img.shields.io/crates/v/markstay)](https://crates.io/crates/markstay)
[![docs.rs](https://img.shields.io/docsrs/markstay)](https://docs.rs/markstay)
[![tests](https://img.shields.io/github/actions/workflow/status/markstaymd/markstay-rs/test.yml?label=tests)](https://github.com/markstaymd/markstay-rs/actions/workflows/test.yml)
[![spec](https://img.shields.io/badge/spec-v1.6-blue)](https://markstay.org)
![no_std](https://img.shields.io/badge/no__std-alloc-orange)
![License](https://img.shields.io/crates/l/markstay)

A fourth, independent implementation of the [markstay spec](https://markstay.org)
(v1.6), in zero-dependency Rust. markstay is a source-level identity primitive for
Markdown blocks: an id token that **stays** bound to its block across edits (marker
`stay:`), so a reference to a block survives the document being rewritten,
including by an LLM.

This is the **parser-free core**: everything string-level and parser-independent
(§8 hashing, §3/§4 marker grammar, §5 blank-line segmentation, §7/§11 lint, §9
quote recovery, §9.1 resolution ladder). It mirrors the Python, JavaScript, and
remark references; all are gated by one shared language-neutral conformance corpus,
which turns "the implementations agree" from an assertion into a tested fact.

Rust is the most *different* of the four targets: statically typed, compiled, UTF-8
native, and explicit about bytes vs `char` vs code points (Python and JS are both
dynamic and GC'd). The §9 algorithm is pinned language-neutrally (Ratcliff/Obershelp
over code points), so the Rust core dropped in against the corpus **without forcing
a spec edit**, which is the strongest available evidence that the standard is
unambiguous rather than defined by one implementation's quirks.

**Child-block identity (§5.5 and §5.6) is not implemented here.** Version 1.3 lets a
direct list item carry its own stay under the reserved `subhash` key; version 1.6
does the same for a GFM table body row. Section 16 makes segmenting and resolving
those **optional**. Its mandatory compatibility layer is parser-free: an exact
`subhash` key, even with an invalid digest, is preserved verbatim, is never given a
container hash, and is never reported as its containing block's stay. The Python
reference implements child segmentation and resolution.

## Why a Rust port (and what it is not)

- **A portability proof.** Two dynamic languages agreeing is weaker than adding a
  systems language that is explicit about the byte/char/code-point distinction §8
  and §9 turn on. Bit-for-bit agreement here (incl. non-BMP `seqmatch` vectors) is
  the real test.
- **A runtime-free artifact.** The library is `no_std` + `alloc` (links no `std`),
  with zero runtime dependencies, so it is also the source for a small WASM module
  and a single static CLI binary that needs no interpreter (Python needs the
  runtime, Node needs Node).
- **Not a speed play.** These documents are tiny; the case is conformance plus a
  genuinely usable, dependency-free artifact, not performance.

## Zero dependencies (the one asymmetry)

Python and JS get SHA-256 and a sequence matcher from their standard libraries.
Rust std ships neither, so to hold the zero-dependency line:

- **SHA-256 is vendored** (`src/sha256.rs`, public domain / FIPS 180-4), not the
  `sha2` crate. Verified against the FIPS empty/`"abc"` vectors and `hash.json`.
- **The Ratcliff/Obershelp `ratio`** (`src/ratio.rs`) is a hand port of CPython
  `difflib`, indexed over `Vec<char>` (code points, never bytes).
- **The marker scanner** (`src/markers.rs`) is hand-rolled, not the `regex` crate.
- **`serde_json` is a dev-dependency only** (the corpus loader in
  `tests/conformance.rs`); it never enters the shipped crate.

## Library

```rust
use markstay as M;

let md = "The ingest stage retries three times.\n<!-- stay:a1b2 -->\n";

// parse into content blocks with lexical marker tokens (§5/§16)
let blocks = M::parse_document(md);

// well-formedness + intra-doc invariants (§7): duplicate/orphan/malformed/drift
let (_blocks, findings) = M::lint_document(md);

// regeneration diff (§11): what an edit did to the ids (dropped/duplicated/moved)
let findings = M::lint_diff(before_md, after_md);

// §8 content hash (ASCII-normalized SHA-256), full or truncated
let h = M::body_hash("some block body", None);

// §9.1 resolution ladder: re-attach ids after an edit, or report detached
let anchors = M::build_anchors(before_md);
let resolutions = M::resolve(&anchors, after_md, M::DEFAULT_THRESHOLD, M::DEFAULT_MARGIN);
// each Resolution.method is "marker" | "hash" | "quote" | "detached"
```

Public API (mirrors the JS `index.js` surface, snake_case): `normalize_body`,
`body_hash`, `ascii_trim`, `find_markers`, `strip_markers`, `fence_state`,
`code_lines`, `strip_markers_outside_code`, `segment_blank_line`,
`parse_document`, `lint_document`, `lint_blocks`, `lint_diff`, `lint_diff_blocks`,
`sort_findings`, `has_errors`, `ratio`, `matching_blocks`, `normalize`,
`quote_ratio`, `body_score`, `context_bonus`, `best_match`, `build_anchors`,
`build_anchors_from_blocks`, `resolve`, `resolve_over_blocks`, plus the
`DEFAULT_THRESHOLD` / `DEFAULT_MARGIN` / `CONTEXT_CHARS` constants. A parsed
`Marker::is_block_stay()` applies §16's containing-block filter.
`build_anchors_from_blocks` / `resolve_over_blocks` are the segmentation-neutral
surfaces (a tree adapter's entry points).

The write path (SPEC.md §6/§7/§8) adds `mint_id`, `format_marker`,
`format_attr_value`, `rewrite_markers`, `stamp`, `restamp`, `repair_duplicates`
(with `StampOptions` / `RestampOptions` and the `DEFAULT_HASH_LENGTH` /
`DEFAULT_ALPHABET` / `DEFAULT_ID_LENGTH` constants). `mint_id` takes an injected
byte source, so the core never calls the OS and stays `no_std`.

The Git-independent commit-check core adds `CommitEntry`, `check_entries`, and
`StagedCheck`. It accepts already-materialized before/after text and returns the
selected baseline pairings, findings, and non-blocking move/deletion notes. Git
process and filesystem I/O stay in the CLI binary, so the library remains
`no_std` + `alloc` and zero-dependency.

## Keeping stays alive through an agent's edit (start here)

Almost every stay that goes missing goes missing the same way: a model rewrote the
document and did not know the markers were load-bearing. The eval measured both
halves of the fix, and they are not close , a naive "clean this up" rewrite keeps
about **5%** of markers, the same rewrite carrying the SPEC.md §11 instruction keeps
**~96-100%**, across five models and three vendors. That outweighs model tier.

`markstay preserve` prints that instruction (`--wrap DOC.md` wraps a document into
a complete editing prompt); `markstay::PRESERVE_INSTRUCTION` and
`markstay::preserve_wrap` are the library equivalents. Pure text composition , no
parsing, no I/O , and byte-identical to the npm and PyPI packages, held there by
the shared conformance corpus rather than by convention.

Everything below is the **backstop**: it catches loss after the fact, it does not
prevent it. Ship the instruction first.

## CLI

A single static binary, suitable as a pre-commit / CI gate. Same subcommand
grammar as the npm and PyPI `markstay` CLIs:

```sh
markstay preserve                     # the §11 instruction for an editing agent
markstay preserve --wrap DOC.md       # that instruction + the doc, as a prompt
markstay lint    FILE...              # well-formedness + intra-doc checks (§7/§8/§10)
markstay lint    --before OLD.md NEW  # regeneration diff (§11)
markstay lint    --json ...           # machine-readable findings
markstay check-staged [FILE...]       # check the staged commit against HEAD (§11)
markstay check-worktree [FILE...]     # check files on disk against HEAD (§11)
markstay stamp   FILE... [-w]         # mint ids for unmarked blocks (§6)
markstay restamp FILE... [-w]         # refresh drifted hashes (§8)
markstay repair  FILE... [-w]         # mint fresh ids for duplicate ids (§7)
```

`lint` and the two check verbs exit non-zero when an error-level finding is
reported. `check-staged` reads the index for a commit hook; `check-worktree` reads
staged, unstaged, and untracked files for a post-edit check. Both stay silent when
there is nothing actionable, resolve recorded renames from `HEAD:<old path>`, and
pair delete/create rewrites with the deleted document sharing the most stay ids.
An id moved to another changed document is a non-blocking note, not a false loss.

The write verbs print the result to stdout by default; `-w`/`--write` edits files
in place (required for more than one file).

`HASH_DRIFT` (a block edited in place) never blocks and is the dominant line in a
normal edit, so the text render **hides it by default** and collapses it to one
receipt (`-> N hash-drift findings hidden (--show-drift to list)`); pass
`--show-drift` to enumerate it. This is presentation only: the finding stays at
`warn` in the `lint_document` / `lint_diff` return values and in `--json` (which is
byte-identical with and without `--show-drift`), so caches and re-embed triggers
that treat a stale hash as fatal read those, unaffected.

```sh
$ cargo run --release -- lint --before old.md new.md
old.md -> new.md:
  [error] DROPPED_ID           -  id b was in the baseline but is gone after the edit (silent loss)
  -> 1 error, 0 warn, 0 info
```

## Layout

```
src/
  ratio.rs     SequenceMatcher.ratio over Unicode code points (the §9 crux)
  sha256.rs    vendored public-domain SHA-256 (FIPS 180-4)
  hash.rs      §8 normalization + SHA-256 body hashing
  text.rs      ASCII whitespace + ASCII case-fold helpers
  markers.rs   §3/§4 strict host-first grammar + discovery + rewrite
  code.rs      §3.3 fenced-code mask (a marker in a fence is content)
  segment.rs   §5 blank-line block segmentation
  parse.rs     §5 document -> content blocks with lexical marker tokens
  lint.rs      §7/§8/§10 lint + §11 regeneration diff
  quote.rs     §9 quote/selector recovery scoring
  resolve.rs   §9.1 resolution ladder (marker -> hash -> quote -> detached)
  id.rs        §6 opaque id minting (injected byte source, no_std)
  stamp.rs     §3/§4/§6/§7/§8 write path (stamp/restamp/repair, marker serialization)
  staged.rs    alloc-only commit checking + stay-id baseline pairing (§11)
  lib.rs       public re-exports
  bin/markstay.rs   thin CLI (preserve/lint/check/write subcommands; Git lives here)
tests/
  conformance.rs    loads conformance/{spec,gen,rows}/*.json (../../conformance)
                    rows is an optional profile this runner declines, and it is
                    loaded so the decline can be asserted rather than assumed
  unit.rs           ports of test_lint.py / test_attach.py / stamp.test.js assertions
```

## Segmentation notes

- **Text inside a fenced code block is content, not markup (§3.3):** a
  marker-shaped string there identifies no block, stays in the hashed body, and
  never makes its block stamped; a writer will not put one there either. That is
  what lets a document *about* markstay show its own markers in examples, which is
  the document the rule exists for. Recognition is a line scan applied once over
  the whole document, on the frontmatter precedent, because neither segmenter has a
  concept of a fence: at most three leading **spaces** (a tab is not one of them,
  since CommonMark expands it against a column model this rule deliberately does
  not have), then three or more backticks or tildes, closed by the first later line
  that is a run of the same character at least as long, followed by nothing but
  spaces and tabs. An unclosed fence runs to the end of the document, and a
  backtick fence's info string may not contain a backtick. **Its limit is stated
  rather than hidden**: a fence indented four spaces or carrying a blockquote or
  list-item prefix on the opening line is invisible to the line scan, so a marker
  after it can go silent. It fails closed (nothing is compared, the block just
  looks unstamped) rather than corrupting anything. **Inline code spans are
  deliberately out of scope**: a marker mangled into a span is still one a tool can
  see, which beats one that has silently stopped existing. **Migration:** a
  document whose body gained a fenced marker-shaped string drifts once and a
  `restamp` clears it; a fence whose only marker was an example is now unstamped
  and takes a fresh stay on the next `stamp`.
- **Leading YAML frontmatter is metadata, not a block (§5.3):** it is skipped before
  segmentation, so it is never a block, never stamped, and never hashed , a
  metadata-only edit (`status: draft` -> `status: done`) must not read as a content
  edit. Recognition is conservative, because `---` is also a thematic break and a
  setext underline: a span counts only when line 1 is exactly `---`, a later line is
  exactly `---` or `...`, the payload between them is non-empty with no blank line,
  and at least one payload line is unambiguously YAML (a `key:` or a `- item`). A
  YAML *comment* does not count, since `# x` is also an ATX heading. "Unambiguously
  YAML" is judged with ASCII whitespace, as everywhere else in the spec (§8/§9): the
  runtimes' own Unicode whitespace sets disagree with each other, and a rule that
  DELETES a span must not vary by implementation. The conditions confine the
  ambiguity rather than removing it, and the rule does not pretend otherwise: a
  blank-free payload that reads as YAML is *also* ordinary Markdown, whether it is a
  sequence (`---` / `- Keep this` / `---`, a list between two thematic breaks) or a
  mapping (`---` / `title: v` / `---`, a setext heading under one). Both are accepted
  and their content is excluded. **Frontmatter wins**, the same call every mainstream
  Markdown site generator makes on the same bytes. A document that fails any of the
  four conditions (no opening `---`, no closing fence, a blank line in the payload,
  no payload line that reads as YAML) falls through to ordinary Markdown, where the
  worst case is a spurious block and a stray hash-drift warning. A marker an older version stamped onto
  frontmatter usually raises `ORPHAN_MARKER`, and deleting that one marker is the
  whole migration. **Lint before deleting**: with no blank line between the marker
  and the content below it, blank-line segmentation reads one run and binds the
  marker to that content, so it is live rather than orphaned and deleting it drops
  a working id.

## Conformance

`tests/conformance.rs` loads the shared corpus at `../../conformance` (spec/ then
gen/ then the optional profile tiers, never tree/) and recomputes every vector,
comparing with a 1e-9 float tolerance and identical key sets. **420/420 core
corpus vectors pass** (180 hand-authored `spec/` + 240 generated `gen/`, 22 files),
incl. every `seqmatch` vector (143, with non-BMP) to delta 0 and the `stamp`/`mint`
write-path vectors shared with JS/Python. The optional `rows` profile (SPEC.md §5.6
table-row identity, 23 vectors) is **declined**: §16 keeps child segmentation
optional and this implementation does not segment child blocks. The decline is
asserted rather than implicit, so the test fails if the profile vanishes from the
corpus, if its vector count changes, or if a profile it has never heard of turns
up. The `check` category carries 14 commit-shaped inputs and asserts baseline pairings,
findings, moves, Markdown tracking departures, deletion notes, and scope. The
`preserve` category is the odd one: it holds the §11 instruction as plain prose
rather than a computation, so this crate's `const` copy cannot drift from the npm
and PyPI copies without failing here.

```sh
cargo test          # conformance corpus + unit tests
cargo clippy --all-targets
cargo build --release
```

This runner joins the Python and JS runners as a cross-implementation regression
sentinel: any change to any implementation that breaks bit-for-bit agreement fails
one of the three. Re-run all three before any spec edit:

```sh
python3 conformance/run_py.py \
  && (cd impl/js && node --test) \
  && (cd impl/rs && cargo test)
```

## Deferred (not in v1)

- **CommonMark mode (§5.2)** , needs a Markdown parser (`comrak` /
  `pulldown-cmark`), which reopens parser-equivalence and pulls a dependency. Left
  to a tree adapter, as the JS baseline leaves it to `remark-stay`.
- **WASM packaging** , a separate track; WASM is packaging of this corpus-green
  core, not new logic.

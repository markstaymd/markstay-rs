# markstay , Rust reference implementation (v1 core)

[![crates.io](https://img.shields.io/crates/v/markstay)](https://crates.io/crates/markstay)
[![docs.rs](https://img.shields.io/docsrs/markstay)](https://docs.rs/markstay)
[![tests](https://img.shields.io/github/actions/workflow/status/markstaymd/markstay-rs/test.yml?label=tests)](https://github.com/markstaymd/markstay-rs/actions/workflows/test.yml)
[![spec](https://img.shields.io/badge/spec-v1.1-blue)](https://markstay.org)
![no_std](https://img.shields.io/badge/no__std-alloc-orange)
![License](https://img.shields.io/crates/l/markstay)

A fourth, independent implementation of the [markstay spec](https://markstay.org)
(v1.1), in zero-dependency Rust. markstay is a source-level identity primitive for
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

## Install

```sh
cargo add markstay        # library
cargo install markstay    # or the `markstay` CLI (single static binary)
```

Zero runtime dependencies. The core is `#![no_std]` + `alloc` (links no `std`); the
CLI binary and the test suite own `std`.

## Why a Rust port (and what it is not)

- **A portability proof.** Two dynamic languages agreeing is weaker than adding a
  systems language that is explicit about the byte/char/code-point distinction §8
  and §9 turn on. Bit-for-bit agreement here (incl. non-BMP `seqmatch` vectors) is
  the real test.
- **A runtime-free artifact.** The library is `no_std` + `alloc` with zero runtime
  dependencies, so it is also the source for a small WASM module and a single static
  CLI binary that needs no interpreter (Python needs the runtime, Node needs Node).
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

// parse into content blocks with attached markers (§5)
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
`body_hash`, `ascii_trim`, `find_markers`, `strip_markers`, `segment_blank_line`,
`parse_document`, `lint_document`, `lint_blocks`, `lint_diff`, `lint_diff_blocks`,
`sort_findings`, `has_errors`, `ratio`, `matching_blocks`, `normalize`,
`quote_ratio`, `body_score`, `context_bonus`, `best_match`, `build_anchors`,
`build_anchors_from_blocks`, `resolve`, `resolve_over_blocks`, plus the
`DEFAULT_THRESHOLD` / `DEFAULT_MARGIN` / `CONTEXT_CHARS` constants.
`build_anchors_from_blocks` / `resolve_over_blocks` are the segmentation-neutral
surfaces (a tree adapter's entry points).

The write path (SPEC.md §6/§7/§8) adds `mint_id`, `format_marker`,
`format_attr_value`, `rewrite_markers`, `stamp`, `restamp`, `repair_duplicates`
(with `StampOptions` / `RestampOptions` and the `DEFAULT_HASH_LENGTH` /
`DEFAULT_ALPHABET` / `DEFAULT_ID_LENGTH` constants). `mint_id` takes an injected
byte source, so the core never calls the OS and stays `no_std`.

## CLI

A single static binary, suitable as a pre-commit / CI gate. Same subcommand
grammar as the npm and PyPI `markstay` CLIs:

```sh
markstay lint    FILE...              # well-formedness + intra-doc checks (§7/§8/§10)
markstay lint    --before OLD.md NEW  # regeneration diff (§11)
markstay lint    --json ...           # machine-readable findings
markstay stamp   FILE... [-w]         # mint ids for unmarked blocks (§6)
markstay restamp FILE... [-w]         # refresh drifted hashes (§8)
markstay repair  FILE... [-w]         # mint fresh ids for duplicate ids (§7)
```

`lint` exits non-zero when any error-level finding is reported. The write verbs
print the result to stdout by default; `-w`/`--write` edits files in place
(required for more than one file).

```sh
$ markstay lint --before old.md new.md
old.md -> new.md:
  [error] DROPPED_ID           -  id b was in the baseline but is gone after the edit (silent loss)
  -> 1 error, 0 warn, 0 info
```

## Conformance

`tests/conformance.rs` loads the vendored corpus at `./conformance` (spec/ then
gen/) and recomputes every vector, comparing with a 1e-9 float tolerance and
identical key sets. **295/295 corpus vectors pass** (66 hand-authored `spec/` + 229
generated `gen/`, 18 files), incl. every `seqmatch` vector (143, with non-BMP) to
delta 0 and the `stamp`/`mint` write-path vectors shared with JS/Python.

```sh
cargo test          # conformance corpus + unit tests
cargo clippy --all-targets
cargo build --release
```

This crate's corpus is a vendored copy of the markstay project's shared corpus, so
`git clone && cargo test` verifies cross-impl conformance standalone. Upstream, the
Rust runner joins the Python and JS runners as a regression sentinel: any change to
any implementation that breaks bit-for-bit agreement fails one of the three.

## Deferred (not in v1)

- **CommonMark mode (§5.2)** , needs a Markdown parser, which reopens
  parser-equivalence and pulls a dependency. Left to a tree adapter, as the JS
  baseline leaves it to `remark-stay`.
- **WASM packaging** , a separate track; WASM is packaging of this corpus-green
  core, not new logic.

## License

MIT. The vendored SHA-256 (`src/sha256.rs`) is public domain (FIPS 180-4).

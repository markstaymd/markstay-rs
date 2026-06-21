//! markstay Rust reference implementation, public API (SPEC.md v1.1, parser-free
//! core). Mirrors the JS reference surface (impl/js/src/index.js) and the Python
//! reference (linter/markstay_lint.py and eval/attachment/{quote,resolver}.py).
//!
//! Zero runtime dependencies: SHA-256 is vendored (src/sha256.rs, public domain)
//! and the marker scanner is hand-rolled, so the crate pulls nothing for the core
//! (serde_json is test-only). CommonMark mode (§5.2) is deferred from v1.
//!
//! `no_std` + `alloc`: the core links no `std`, only `alloc` (the WASM handover,
//! HANDOVER_RUST_WASM.md, assumes this stays open). The CLI binary and the tests
//! own `std`.

#![no_std]

extern crate alloc;

pub mod hash;
pub mod id;
pub mod lint;
pub mod markers;
pub mod parse;
pub mod quote;
pub mod ratio;
pub mod resolve;
pub mod segment;
pub mod sha256;
pub mod stamp;
pub mod text;

// --- public API re-exports (snake_case mirror of the JS surface) -------------

pub use hash::{body_hash, normalize_body};
pub use text::ascii_trim;

pub use markers::{find_markers, rewrite_markers, strip_markers, Marker, Syntax};
pub use segment::segment_blank_line;
pub use parse::{parse_document, Block};

pub use id::{is_id_charset, mint_id, DEFAULT_ALPHABET, DEFAULT_ID_LENGTH};
pub use stamp::{
    format_attr_value, format_marker, repair_duplicates, restamp, stamp, FormatError, Minted,
    Renamed, RepairResult, RestampOptions, RestampResult, StampOptions, StampResult,
    DEFAULT_HASH_LENGTH,
};

pub use lint::{
    has_errors, lint_blocks, lint_diff, lint_diff_blocks, lint_document, sort_findings, Finding,
    Level,
};

pub use ratio::{matching_blocks, ratio};

pub use quote::{
    best_match, body_score, context_bonus, normalize, quote_ratio, BestMatch, Selector,
    CONTEXT_CHARS,
};

pub use resolve::{
    build_anchors, build_anchors_from_blocks, resolve, resolve_over_blocks, Anchor, Resolution,
    DEFAULT_MARGIN, DEFAULT_THRESHOLD,
};

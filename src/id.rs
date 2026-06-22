// Opaque id generation (SPEC.md §6). Port of impl/js/src/id.js (`mintId`,
// `DEFAULT_ALPHABET`, `DEFAULT_ID_LENGTH`, `ID_CHARSET`), which ports the Python
// reference (impl/py/src/markstay/id.py). The three are gated by the shared
// conformance corpus.
//
// The reference write path mints "a short opaque generated id, not derived from
// the block text," so a rewriting model has nothing to "improve." Generation is
// the only randomness in the core; every write helper funnels its minting through
// an injectable byte source so the conformance/unit tests stay deterministic AND
// the core stays `no_std` (the OS RNG lives in the CLI binary, which owns `std`).

use alloc::string::String;
use alloc::vec::Vec;

/// Default id alphabet: base62, a strict subset of the §6 id charset
/// `[A-Za-z0-9_-]`. `_` and `-` are legal in authored ids but omitted from
/// *generated* ids so a minted id never begins with `-` (which reads as a CLI
/// flag) and never collides with the marker delimiters.
pub const DEFAULT_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// 8 base62 chars ~= 47.6 bits: ample collision resistance for per-document
/// coverage without the token weight of a UUID (§6 calls UUIDs too heavy).
pub const DEFAULT_ID_LENGTH: usize = 8;

/// True iff `s` matches the §6 id grammar `^[A-Za-z0-9_-]+$` (non-empty run of
/// `[A-Za-z0-9_-]`). The Rust core has no regex; this is the predicate form of
/// the JS/Python `ID_CHARSET` regex.
#[inline]
pub fn is_id_charset(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(crate::markers::is_id_byte)
}

/// Mint one opaque id (SPEC.md §6).
///
/// * `length`   id length in characters (must be >= 1)
/// * `alphabet` characters to draw from (must have >= 2 chars; ASCII for a
///   generated id, though any non-empty char set works)
/// * `random`   `n -> bytes` source; injectable so write helpers can be made
///   deterministic in tests and so the core never calls the OS (the CLI passes a
///   `/dev/urandom`-backed source).
///
/// Bytes are drawn with rejection sampling so the alphabet is unbiased even when
/// its length does not divide 256. Panics on a degenerate `length`/`alphabet`,
/// mirroring the JS/Python `throw`/`raise`.
pub fn mint_id(length: usize, alphabet: &str, mut random: impl FnMut(usize) -> Vec<u8>) -> String {
    assert!(length >= 1, "mint_id: length must be a positive integer");
    let alpha: Vec<char> = alphabet.chars().collect();
    let n = alpha.len();
    assert!(n >= 2, "mint_id: alphabet needs at least 2 characters");
    let limit = 256 - (256 % n); // largest unbiased byte threshold
    let mut out = String::new();
    let mut count = 0usize;
    while count < length {
        let buf = random(length - count);
        for &b in &buf {
            if count >= length {
                break;
            }
            if (b as usize) < limit {
                out.push(alpha[(b as usize) % n]);
                count += 1;
            }
        }
    }
    out
}

// Cross-implementation conformance: run the shared language-neutral corpus
// (conformance/spec/ + conformance/gen/) against the Rust implementation. The
// Python runner (conformance/run_py.py) and the JS runner
// (impl/js/test/conformance.test.js) assert the same vectors against their
// implementations, so the three together are the cross-impl regression sentinel:
// any change that breaks agreement fails one of them.
//
// The contract reproduced here is pinned in PLAN_RUST_IMPL.md "Conformance
// harness contract": load spec/ then gen/ (never tree/), unknown category is a
// failure, equality is `approx` (1e-9 float tolerance, identical key sets,
// booleans never coerced through the numeric path).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use markstay::{
    best_match, body_hash, body_score, build_anchors, context_bonus, find_markers, lint_diff,
    lint_document, matching_blocks, mint_id, normalize_body, parse_document, quote_ratio, ratio,
    repair_duplicates, resolve, restamp, sort_findings, stamp, Block, Finding, Marker,
    RestampOptions, Selector, StampOptions, Syntax, DEFAULT_ALPHABET, DEFAULT_HASH_LENGTH,
};

const TOL: f64 = 1e-9;

/// Deep equality with a 1e-9 float tolerance, mirroring run_py.py:approx.
/// Booleans compare exactly (never through the numeric path); object key sets
/// must be identical.
fn approx(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Bool(_), _) | (_, Value::Bool(_)) => false,
        (Value::Number(x), Value::Number(y)) => {
            let (xf, yf) = (x.as_f64(), y.as_f64());
            match (xf, yf) {
                (Some(xf), Some(yf)) => (xf - yf).abs() < TOL,
                _ => x == y,
            }
        }
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| approx(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.keys()
                    .all(|k| y.contains_key(k) && approx(&x[k], &y[k]))
        }
        _ => a == b,
    }
}

// --- canonical shapes (mirror generate.py's *_dict helpers) -----------------

fn marker_value(m: &Marker) -> Value {
    json!({
        "id": m.id,
        "hash": m.hash,
        "raw": m.raw,
        "syntax": m.syntax.as_str(),
        "line": m.line,
        "malformed": m.malformed,
    })
}

fn block_value(b: &Block) -> Value {
    let ids: Vec<Value> = b
        .markers
        .iter()
        .map(|m| match &m.id {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        })
        .collect();
    json!({
        "content": b.content,
        "index": b.index,
        "ids": ids,
        "line": b.line,
        "orphan": b.index == -1,
    })
}

fn finding_value(f: &Finding, with_line: bool) -> Value {
    if with_line {
        json!({ "level": f.level.as_str(), "code": f.code, "id": f.id, "line": f.line })
    } else {
        json!({ "level": f.level.as_str(), "code": f.code, "id": f.id })
    }
}

fn mb_value(blocks: &[(usize, usize, usize)]) -> Value {
    Value::Array(
        blocks
            .iter()
            .map(|t| json!([t.0, t.1, t.2]))
            .collect(),
    )
}

fn str_field<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str()
        .unwrap_or_else(|| panic!("vector field {:?} is not a string: {}", k, v))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("expected array")
        .iter()
        .map(|x| x.as_str().expect("expected string").to_string())
        .collect()
}

// --- per-category verifiers: (vector) -> (got, want) ------------------------

fn verify(category: &str, v: &Value) -> (Value, Value) {
    match category {
        "hash" => {
            let body = str_field(v, "body");
            let mut trunc = Map::new();
            for (k, _) in v["truncations"].as_object().expect("truncations object") {
                let n: usize = k.parse().expect("truncation key is a number");
                trunc.insert(k.clone(), json!(body_hash(body, Some(n))));
            }
            let got = json!({
                "normalized": normalize_body(body),
                "sha256": body_hash(body, None),
                "truncations": Value::Object(trunc),
            });
            let want = json!({
                "normalized": v["normalized"],
                "sha256": v["sha256"],
                "truncations": v["truncations"],
            });
            (got, want)
        }
        "markers" => {
            let got = Value::Array(
                find_markers(str_field(v, "text"), 0)
                    .iter()
                    .map(marker_value)
                    .collect(),
            );
            (got, v["markers"].clone())
        }
        "parse" => {
            let got = Value::Array(
                parse_document(str_field(v, "doc"))
                    .iter()
                    .map(block_value)
                    .collect(),
            );
            (got, v["blocks"].clone())
        }
        "lint" => {
            let (_, findings) = lint_document(str_field(v, "doc"));
            let got = Value::Array(
                sort_findings(&findings)
                    .iter()
                    .map(|f| finding_value(f, true))
                    .collect(),
            );
            (got, v["findings"].clone())
        }
        "diff" => {
            let findings = lint_diff(str_field(v, "before"), str_field(v, "after"));
            let got = Value::Array(
                sort_findings(&findings)
                    .iter()
                    .map(|f| finding_value(f, false))
                    .collect(),
            );
            (got, v["findings"].clone())
        }
        "seqmatch" => {
            let (a, b) = (str_field(v, "a"), str_field(v, "b"));
            let got = json!({
                "ratio": ratio(a, b),
                "matching_blocks": mb_value(&matching_blocks(a, b)),
            });
            let want = json!({ "ratio": v["ratio"], "matching_blocks": v["matching_blocks"] });
            (got, want)
        }
        "score" => verify_score(v),
        "resolve" => {
            let anchors = build_anchors(str_field(v, "before"));
            let res = resolve(
                &anchors,
                str_field(v, "after"),
                v["threshold"].as_f64().expect("threshold"),
                v["margin"].as_f64().expect("margin"),
            );
            let mut got = Map::new();
            for r in &res {
                got.insert(
                    r.id.clone(),
                    json!({ "method": r.method, "target": r.target, "score": r.score }),
                );
            }
            (Value::Object(got), v["resolutions"].clone())
        }
        "stamp" => verify_stamp(v),
        "mint" => verify_mint(v),
        other => panic!("unknown category {:?}", other),
    }
}

/// Id-minting vectors (§6). A fixed byte array is the injected source, consumed
/// in order across the rejection loop's `random(n)` draws, so the rejection
/// sampling is exercised identically in all three impls (a forced-rejection
/// vector is the proof the Rust port matches JS/Python byte-for-byte).
fn verify_mint(v: &Value) -> (Value, Value) {
    let length = v["length"].as_u64().expect("mint vector needs a length") as usize;
    let alphabet = v["alphabet"].as_str().unwrap_or(DEFAULT_ALPHABET).to_string();
    let bytes: Vec<u8> = v["bytes"]
        .as_array()
        .expect("mint vector needs a bytes array")
        .iter()
        .map(|x| x.as_u64().expect("byte is an integer") as u8)
        .collect();
    let mut cursor = 0usize;
    let random = |n: usize| {
        let out = bytes[cursor..cursor + n].to_vec();
        cursor += n;
        out
    };
    (json!(mint_id(length, &alphabet, random)), v["expected"].clone())
}

/// A deterministic id factory yielding `ids[0], ids[1], ...` (mirrors the JS
/// `seqFactory`): the write helpers' collision-avoidance wraps this, so the mint
/// path itself is never exercised by these vectors.
fn seq_minter(ids: Vec<String>) -> impl FnMut() -> String {
    let mut i = 0usize;
    move || {
        let s = ids[i].clone();
        i += 1;
        s
    }
}

/// Write-path vectors (§3/§4/§6/§7/§8), dispatched by the per-vector `op`. The id
/// sequence is injected so minting is deterministic; `expected` is the frozen
/// oracle shared with the JS and Python runners.
fn verify_stamp(v: &Value) -> (Value, Value) {
    let input = str_field(v, "input");
    let o = &v["options"];
    let got = match str_field(v, "op") {
        "stamp" => {
            let opts = StampOptions {
                syntax: match o["syntax"].as_str() {
                    Some("mdx") => Syntax::Mdx,
                    _ => Syntax::Html,
                },
                hash: o["hash"].as_bool().unwrap_or(true),
                hash_length: o["hashLength"]
                    .as_u64()
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_HASH_LENGTH),
            };
            let r = stamp(input, &opts, seq_minter(strings(&v["ids"])));
            let minted: Vec<Value> = r
                .minted
                .iter()
                .map(|m| json!({ "id": m.id, "line": m.line }))
                .collect();
            json!({ "text": r.text, "minted": minted })
        }
        "restamp" => {
            let opts = RestampOptions {
                hash_length: o["hashLength"].as_u64().map(|n| n as usize),
                add_missing: o["addMissing"].as_bool().unwrap_or(false),
            };
            let r = restamp(input, &opts);
            json!({ "text": r.text, "refreshed": r.refreshed })
        }
        "repair" => {
            let r = repair_duplicates(input, seq_minter(strings(&v["ids"])));
            let renamed: Vec<Value> = r
                .renamed
                .iter()
                .map(|x| json!({ "from": x.from, "to": x.to }))
                .collect();
            json!({ "text": r.text, "renamed": renamed })
        }
        other => panic!("unknown stamp op {:?}", other),
    };
    (got, v["expected"].clone())
}

fn verify_score(v: &Value) -> (Value, Value) {
    match str_field(v, "fn") {
        "ratio" => (
            json!(quote_ratio(str_field(v, "a"), str_field(v, "b"))),
            v["score"].clone(),
        ),
        "body_score" => {
            let sel = Selector {
                quote: str_field(v, "quote").to_string(),
                ..Default::default()
            };
            (
                json!(body_score(&sel, str_field(v, "candidate"))),
                v["score"].clone(),
            )
        }
        "context_bonus" => {
            let sel = Selector {
                quote: "q".to_string(),
                prefix: str_field(v, "prefix").to_string(),
                suffix: str_field(v, "suffix").to_string(),
            };
            (
                json!(context_bonus(&sel, str_field(v, "prev"), str_field(v, "next"))),
                v["bonus"].clone(),
            )
        }
        "best_match" => {
            let sel = Selector {
                quote: str_field(v, "quote").to_string(),
                prefix: str_field(v, "prefix").to_string(),
                suffix: str_field(v, "suffix").to_string(),
            };
            let cands = strings(&v["candidates"]);
            let bm = best_match(&sel, &cands);
            let got = json!({ "index": bm.index, "score": bm.score, "runner_up": bm.runner_up });
            let want = json!({
                "index": v["index"],
                "score": v["score"],
                "runner_up": v["runner_up"],
            });
            (got, want)
        }
        other => panic!("unknown score fn {:?}", other),
    }
}

fn corpus_dir() -> PathBuf {
    // The shared conformance corpus. In the umbrella crate (impl/rs) it is the
    // tree two levels up; the published mirror vendors its own copy and the sync
    // script rewrites the join target so `cargo test` runs standalone after a
    // plain `git clone`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance")
        .canonicalize()
        .expect("conformance dir resolves")
}

fn corpus_files() -> Vec<(String, PathBuf)> {
    let root = corpus_dir();
    let mut files = Vec::new();
    for tier in ["spec", "gen"] {
        let dir = root.join(tier);
        let mut names: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {}", dir.display(), e))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        names.sort();
        for p in names {
            files.push((tier.to_string(), p));
        }
    }
    files
}

#[test]
fn corpus() {
    let files = corpus_files();
    assert!(
        !files.is_empty(),
        "no corpus files found under conformance/spec or conformance/gen"
    );

    let mut total = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (tier, path) in &files {
        let data: Value = serde_json::from_str(&fs::read_to_string(path).expect("read corpus file"))
            .expect("parse corpus json");
        let category = data["category"].as_str().expect("category string");
        for v in data["vectors"].as_array().expect("vectors array") {
            total += 1;
            let name = v["name"].as_str().unwrap_or("?");
            let (got, want) = verify(category, v);
            if !approx(&got, &want) {
                failures.push(format!(
                    "FAIL {}/{}:{}\n     got  = {}\n     want = {}",
                    tier, category, name, got, want
                ));
            }
        }
    }

    let passed = total - failures.len();
    println!(
        "\n{}/{} corpus vectors pass ({} files)",
        passed,
        total,
        files.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} vectors failed:\n{}",
        failures.len(),
        total,
        failures.join("\n")
    );
}

// Cost of SPEC.md §11's relocation scan (v1.11), ported from
// linter/test_relocation_cost.py. N kept ids, each block rewritten in a
// vocabulary the old text does not share (so clause 1 holds for every id and
// the window is always scanned), and RUN unmarked blocks of the old vocabulary
// and length typed directly above each marker. A document-wide scan would score
// every drifted id against all N * RUN free blocks; the window scores each
// against its own RUN. The documents come from the same fixed LCG as the other
// cores' tests. The crate's internals cannot be counted from here, so this
// asserts wall time: doubling N should roughly double it, where a quadratic
// scan quadruples it.

use std::time::Instant;

use markstay::lint_diff;

const WORDS: &str = "apple orchard fence river stone market window lantern harbor meadow \
     copper silver ledger anchor thistle bramble cedar willow quarry summit \
     garden basket candle mirror saddle kettle needle pebble ribbon timber";
const OTHER: &str = "zebra quokka yak vulture jaguar ibex hyena gnu ferret dingo \
     koala lemur mink narwhal ocelot puma quail raven sloth tapir";
const RUN: usize = 2;

fn relocation_docs(n: usize, run: usize) -> (String, String) {
    let words: Vec<&str> = WORDS.split(' ').collect();
    let other: Vec<&str> = OTHER.split(' ').collect();
    let mut x = n as u32;
    let mut para = |w: &[&str]| -> String {
        let mut out = Vec::with_capacity(30);
        for _ in 0..30 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
            out.push(w[((x >> 16) as usize) % w.len()]);
        }
        let s = out.join(" ");
        let mut c = s.chars();
        let first = c.next().unwrap().to_ascii_uppercase();
        format!("{}{}.", first, c.as_str())
    };
    let ids: Vec<String> = (0..n).map(|k| format!("i{:05}", k)).collect();
    let mut before = String::new();
    for i in &ids {
        before.push_str(&format!("{}\n<!-- stay:{} -->\n\n", para(&words), i));
    }
    let mut after = String::new();
    for i in &ids {
        for _ in 0..run {
            after.push_str(&format!("{}\n\n", para(&words)));
        }
        after.push_str(&format!("{}\n<!-- stay:{} -->\n\n", para(&other), i));
    }
    (before, after)
}

fn best(n: usize) -> f64 {
    let (before, after) = relocation_docs(n, RUN);
    (0..3)
        .map(|_| {
            let t = Instant::now();
            lint_diff(&before, &after);
            t.elapsed().as_secs_f64()
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn every_drifted_id_stays_hash_drift_on_the_cost_shape() {
    let (before, after) = relocation_docs(100, RUN);
    let codes: Vec<&str> = lint_diff(&before, &after).iter().map(|f| f.code).collect();
    assert_eq!(codes, vec!["HASH_DRIFT"; 100]);
}

#[test]
fn the_window_scans_wall_time_roughly_doubles_with_n() {
    let small = best(100);
    let large = best(200);
    // Linear doubles, quadratic quadruples; 3.0 leaves room for a loaded machine.
    assert!(large / small < 3.0, "100: {small}s, 200: {large}s");
}

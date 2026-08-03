// Git-independent core for `check-staged` / `check-worktree`.
//
// The binary materializes Git changes as CommitEntry values. Keeping baseline
// pairing in this alloc-only module lets the shared commit-shaped corpus test the
// same code the CLI uses while preserving the crate's no_std and zero-dependency
// properties.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{has_errors, lint_diff, lint_document, parse_document, Finding};

#[derive(Clone, Copy, Debug)]
pub struct CommitEntry<'a> {
    pub status: char,
    pub source: &'a str,
    pub destination: &'a str,
    /// HEAD text at source for R/C/D, or at destination otherwise.
    pub before: Option<&'a str>,
    /// Index or worktree text at destination; absent only for deletions.
    pub after: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselinePairing {
    pub path: String,
    pub baseline: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CheckReport {
    pub label: String,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default)]
pub struct StagedCheck {
    pub reports: Vec<CheckReport>,
    pub notes: Vec<String>,
    pub pairings: Vec<BaselinePairing>,
}

impl StagedCheck {
    pub fn has_errors(&self) -> bool {
        self.reports.iter().any(|report| has_errors(&report.findings))
    }
}

pub fn is_markdown(path: &str) -> bool {
    if path == ".markstay" || path.starts_with(".markstay/") {
        return false;
    }
    path.ends_with(".md") || path.ends_with(".markdown")
}

fn ids_of(text: Option<&str>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let Some(text) = text else {
        return ids;
    };
    for block in parse_document(text) {
        if block.index < 0 {
            continue;
        }
        for marker in block.markers {
            if !marker.malformed {
                if let Some(id) = marker.id {
                    ids.insert(id);
                }
            }
        }
    }
    ids
}

/// Check commit-shaped entries without reading Git or the filesystem.
///
/// `scope` narrows reports only. The full input still participates in baseline
/// pairing and cross-document move detection. `pairings` includes clean files so
/// conformance tests can assert the selected baseline directly.
pub fn check_entries(entries: &[CommitEntry<'_>], scope: &[String]) -> StagedCheck {
    let changed: Vec<&CommitEntry<'_>> = entries
        .iter()
        .filter(|entry| entry.status != 'D' && is_markdown(entry.destination))
        .collect();
    let deleted: Vec<&CommitEntry<'_>> = entries
        .iter()
        .filter(|entry| {
            (entry.status == 'D' && is_markdown(entry.source))
                || (entry.status == 'R'
                    && is_markdown(entry.source)
                    && !is_markdown(entry.destination))
        })
        .collect();
    let mut result = StagedCheck::default();
    if changed.is_empty() && deleted.is_empty() {
        return result;
    }

    let staged_ids: Vec<(&str, BTreeSet<String>)> = changed
        .iter()
        .map(|entry| (entry.destination, ids_of(Some(entry.after.unwrap_or("")))))
        .collect();
    let mut committed_ids = BTreeSet::new();
    for (_, ids) in &staged_ids {
        committed_ids.extend(ids.iter().cloned());
    }

    let deleted_ids: Vec<(&str, BTreeSet<String>)> =
        deleted.iter().map(|entry| (entry.source, ids_of(entry.before))).collect();
    let mut claimed = alloc::vec![false; deleted.len()];

    for entry in changed {
        let staged = entry.after.unwrap_or("");
        let mine = staged_ids
            .iter()
            .find(|(path, _)| *path == entry.destination)
            .map(|(_, ids)| ids)
            .expect("changed entry has staged ids");

        let (mut baseline, mut origin): (Option<String>, Option<String>) = (None, None);
        if entry.status == 'R' && is_markdown(entry.source) && entry.before.is_some() {
            baseline = entry.before.map(ToString::to_string);
            origin = Some(entry.source.to_string());
        } else if !matches!(entry.status, 'C' | 'R') && entry.before.is_some() {
            baseline = entry.before.map(ToString::to_string);
            origin = Some(entry.destination.to_string());
        } else {
            let mut best: Option<usize> = None;
            let mut best_n = 0usize;
            for (i, (_, candidate_ids)) in deleted_ids.iter().enumerate() {
                if claimed[i] {
                    continue;
                }
                let n = mine.intersection(candidate_ids).count();
                if n > best_n {
                    best = Some(i);
                    best_n = n;
                }
            }
            if let Some(i) = best {
                claimed[i] = true;
                baseline = deleted[i].before.map(ToString::to_string);
                origin = Some(deleted[i].source.to_string());
            }
        }

        result.pairings.push(BaselinePairing {
            path: entry.destination.to_string(),
            baseline: origin.clone(),
        });

        let (_, mut findings) = lint_document(staged);
        if let Some(before) = baseline.as_deref() {
            findings.extend(lint_diff(before, staged));
        }

        let mut kept = Vec::new();
        for finding in findings {
            let moved = finding.code == "DROPPED_ID"
                && finding.id.as_ref().map(|id| committed_ids.contains(id)).unwrap_or(false);
            if moved {
                let id = finding.id.as_deref().unwrap_or("");
                let elsewhere: BTreeSet<String> = staged_ids
                    .iter()
                    .filter(|(path, ids)| *path != entry.destination && ids.contains(id))
                    .map(|(path, _)| (*path).to_string())
                    .collect();
                result.notes.push(format!(
                    "{}: moved out of {} into {} (still in this commit, not blocking)",
                    id,
                    origin.as_deref().unwrap_or(entry.destination),
                    elsewhere.into_iter().collect::<Vec<_>>().join(", ")
                ));
            } else {
                kept.push(finding);
            }
        }

        if !scope.is_empty() && !scope.iter().any(|path| path == entry.destination) {
            continue;
        }
        if !kept.is_empty() {
            let label = match origin.as_deref() {
                Some(path) if path != entry.destination => {
                    format!("{} (baseline {})", entry.destination, path)
                }
                _ => entry.destination.to_string(),
            };
            result.reports.push(CheckReport { label, findings: kept });
        }
    }

    for (i, entry) in deleted.iter().enumerate() {
        if claimed[i] {
            continue;
        }
        let gone: Vec<String> =
            deleted_ids[i].1.iter().filter(|id| !committed_ids.contains(*id)).cloned().collect();
        if !gone.is_empty() {
            let mut shown = gone.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            if gone.len() > 6 {
                shown.push_str(", ...");
            }
            if entry.status == 'R' {
                result.notes.push(format!(
                    "{}: renamed to {}, leaving Markdown tracking with {} stay(s) not carried by another Markdown file ({})",
                    entry.source,
                    entry.destination,
                    gone.len(),
                    shown
                ));
            } else {
                result.notes.push(format!(
                    "{}: deleted with {} stay(s) that no staged file carries ({})",
                    entry.source,
                    gone.len(),
                    shown
                ));
            }
        }
    }

    result
}

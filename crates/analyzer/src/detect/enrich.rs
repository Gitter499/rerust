//! Commit-history enrichment for rewrite metrics.
//!
//! Prefer [`super::transitions::analyze_history`] for concrete language-shift
//! detection. This module remains for backwards compatibility with
//! `--enrich-commits` and exposes shared AI-assist heuristics.

use std::time::Duration;

use tracing::warn;

use super::git_history::{fetch_log, parse_numstat_log, CommitRecord};

/// Metrics derived from commit history during a rewrite window.
#[derive(Debug, Clone, Default)]
pub struct CommitEnrichment {
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    pub rewrite_velocity: Option<f64>,
    /// Experimental 0.0–1.0 heuristic for signs of AI-assisted translation.
    pub ai_assist_score: Option<f64>,
    pub rewrite_duration_days: Option<u32>,
    pub commit_count: Option<u32>,
}

/// Legacy per-commit shape used by the AI-assist heuristic.
#[derive(Debug, Clone)]
pub(crate) struct LegacyCommitStat {
    pub timestamp: i64,
    pub subject: String,
    pub added: u64,
    pub removed: u64,
    pub rust_added: u64,
    pub non_rust_removed: u64,
}

pub(crate) fn legacy_stat_from_record(c: &CommitRecord) -> LegacyCommitStat {
    let mut added = 0u64;
    let mut removed = 0u64;
    let mut rust_added = 0u64;
    let mut non_rust_removed = 0u64;
    for f in &c.files {
        added += f.added;
        removed += f.removed;
        if f.path.ends_with(".rs") {
            rust_added += f.added;
        } else if f.removed > 0 {
            non_rust_removed += f.removed;
        }
    }
    LegacyCommitStat {
        timestamp: c.timestamp,
        subject: c.subject.clone(),
        added,
        removed,
        rust_added,
        non_rust_removed,
    }
}

const REWRITE_MSG_TERMS: &[&str] = &[
    "rewrite",
    "rewritten",
    "port",
    "ported",
    "translate",
    "translated",
    "reimplement",
    "migration",
    "rust",
    "convert",
];

const AI_MSG_PATTERNS: &[&str] = &[
    "refactor",
    "translate",
    "port module",
    "porting",
    "convert to rust",
    "migrate",
    "translation",
];

/// Shallow-clone `repo_url` and analyze recent commits (legacy path).
pub async fn enrich_commits(repo_url: &str, per_step_timeout: Duration) -> CommitEnrichment {
    let tmp = match tempfile::Builder::new()
        .prefix("rerust-enrich-")
        .tempdir()
    {
        Ok(t) => t,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "enrich: could not create temp dir");
            return CommitEnrichment::default();
        }
    };

    let repo_budget = per_step_timeout.saturating_mul(3);
    let log = match fetch_log(repo_url, tmp.path(), per_step_timeout, repo_budget, false).await {
        Some(s) => s,
        None => return CommitEnrichment::default(),
    };

    let commits = parse_numstat_log(&log);
    let window: Vec<LegacyCommitStat> = commits
        .iter()
        .map(legacy_stat_from_record)
        .filter(|c| is_rewrite_commit(c))
        .collect();

    enrichment_from_legacy_window(&window)
}

fn is_rewrite_commit(c: &LegacyCommitStat) -> bool {
    let subj = c.subject.to_lowercase();
    let msg_match = REWRITE_MSG_TERMS.iter().any(|t| subj.contains(t));
    let file_match = c.rust_added > 0 && c.non_rust_removed > 0;
    msg_match || file_match || (c.rust_added > 50 && c.removed > 50)
}

fn enrichment_from_legacy_window(window: &[LegacyCommitStat]) -> CommitEnrichment {
    if window.is_empty() {
        return CommitEnrichment::default();
    }

    let lines_added: u64 = window.iter().map(|c| c.added).sum();
    let lines_removed: u64 = window.iter().map(|c| c.removed).sum();
    let commit_count = window.len() as u32;

    let timestamps: Vec<i64> = window.iter().map(|c| c.timestamp).collect();
    let min_ts = *timestamps.iter().min().unwrap_or(&0);
    let max_ts = *timestamps.iter().max().unwrap_or(&0);
    let duration_days = duration_days(min_ts, max_ts);

    let velocity = if duration_days > 0 {
        Some(round2(
            (lines_added + lines_removed) as f64 / duration_days as f64,
        ))
    } else if lines_added + lines_removed > 0 {
        Some((lines_added + lines_removed) as f64)
    } else {
        None
    };

    let refs: Vec<&LegacyCommitStat> = window.iter().collect();
    let ai_score = Some(compute_ai_assist_score_legacy(&refs, lines_added, lines_removed));

    CommitEnrichment {
        lines_added: Some(lines_added),
        lines_removed: Some(lines_removed),
        rewrite_velocity: velocity,
        ai_assist_score: ai_score,
        rewrite_duration_days: Some(duration_days.max(1)),
        commit_count: Some(commit_count),
    }
}

pub(crate) fn compute_ai_assist_score_legacy(
    window: &[&LegacyCommitStat],
    lines_added: u64,
    lines_removed: u64,
) -> f64 {
    if window.is_empty() {
        return 0.0;
    }

    let mut score = 0.0f64;
    let n = window.len() as f64;
    let total_lines = (lines_added + lines_removed) as f64;

    let avg_loc = total_lines / n;
    if avg_loc > 500.0 {
        score += 0.30;
    } else if avg_loc > 200.0 {
        score += 0.15;
    }

    let templated = window
        .iter()
        .filter(|c| {
            let s = c.subject.to_lowercase();
            AI_MSG_PATTERNS.iter().any(|p| s.contains(p))
        })
        .count() as f64;
    let templated_ratio = templated / n;
    score += (templated_ratio * 0.25).min(0.25);

    let big_commits = window.iter().filter(|c| c.added > 2000).count() as f64;
    if big_commits > 0.0 {
        score += (big_commits / n * 0.25).min(0.25);
    }

    score += burst_score(window).min(0.25);

    round2(score.clamp(0.0, 1.0))
}

fn burst_score(window: &[&LegacyCommitStat]) -> f64 {
    if window.len() < 3 {
        return 0.0;
    }
    let mut sorted: Vec<&LegacyCommitStat> = window.to_vec();
    sorted.sort_by_key(|c| c.timestamp);

    let mut max_burst = 1usize;
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i + 1;
        while j < sorted.len() && sorted[j].timestamp - sorted[i].timestamp <= 48 * 3600 {
            j += 1;
        }
        max_burst = max_burst.max(j - i);
        i += 1;
    }

    if max_burst >= 10 {
        0.25
    } else if max_burst >= 5 {
        0.15
    } else if max_burst >= 3 {
        0.08
    } else {
        0.0
    }
}

fn duration_days(min_ts: i64, max_ts: i64) -> u32 {
    use chrono::{TimeZone, Utc};
    if min_ts == 0 || max_ts == 0 {
        return 1;
    }
    match (
        Utc.timestamp_opt(min_ts, 0).single(),
        Utc.timestamp_opt(max_ts, 0).single(),
    ) {
        (Some(s), Some(e)) => (e - s).num_days().max(0) as u32,
        _ => 1,
    }
    .max(1)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str, added: u64, removed: u64, ts: i64) -> LegacyCommitStat {
        LegacyCommitStat {
            timestamp: ts,
            subject: subject.into(),
            added,
            removed,
            rust_added: if added > 0 { added } else { 0 },
            non_rust_removed: removed,
        }
    }

    #[test]
    fn high_loc_and_templated_messages_raise_ai_score() {
        let c1 = commit("refactor: translate module foo", 3000, 2800, 1_700_000_000);
        let c2 = commit("refactor: port module bar", 2500, 2400, 1_700_086_400);
        let refs = [&c1, &c2];
        let score = compute_ai_assist_score_legacy(&refs, 5500, 5200);
        assert!(score >= 0.5, "expected high score, got {score}");
    }

    #[test]
    fn small_manual_commits_score_low() {
        let c1 = commit("fix typo", 3, 1, 1_700_000_000);
        let c2 = commit("update readme", 10, 5, 1_700_100_000);
        let refs = [&c1, &c2];
        let score = compute_ai_assist_score_legacy(&refs, 13, 6);
        assert!(score < 0.3, "expected low score, got {score}");
    }
}

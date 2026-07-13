//! Rewrite-window metrics and the experimental AI-assist heuristic.

use super::git_history::CommitRecord;

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

const AI_MSG_PATTERNS: &[&str] = &[
    "refactor",
    "translate",
    "port module",
    "porting",
    "convert to rust",
    "migrate",
    "translation",
];

fn commit_added(c: &CommitRecord) -> u64 {
    c.files.iter().map(|f| f.added).sum()
}

/// Build enrichment from window totals + optional AI score input.
pub(crate) fn enrichment_from_totals(
    lines_added: u64,
    lines_removed: u64,
    commit_count: u32,
    min_ts: i64,
    max_ts: i64,
    ai_window: &[&CommitRecord],
) -> CommitEnrichment {
    let days = duration_days(min_ts, max_ts);
    let churn = lines_added + lines_removed;
    let velocity = if days > 0 {
        Some(round2(churn as f64 / days as f64))
    } else if churn > 0 {
        Some(churn as f64)
    } else {
        None
    };

    CommitEnrichment {
        lines_added: Some(lines_added),
        lines_removed: Some(lines_removed),
        rewrite_velocity: velocity,
        ai_assist_score: Some(compute_ai_assist_score(
            ai_window,
            lines_added,
            lines_removed,
        )),
        rewrite_duration_days: Some(days.max(1)),
        commit_count: Some(commit_count),
    }
}

pub(crate) fn compute_ai_assist_score(
    window: &[&CommitRecord],
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
    score += ((templated / n) * 0.25).min(0.25);

    let big_commits = window.iter().filter(|c| commit_added(c) > 2000).count() as f64;
    if big_commits > 0.0 {
        score += (big_commits / n * 0.25).min(0.25);
    }

    score += burst_score(window).min(0.25);
    round2(score.clamp(0.0, 1.0))
}

fn burst_score(window: &[&CommitRecord]) -> f64 {
    if window.len() < 3 {
        return 0.0;
    }
    let mut sorted: Vec<&CommitRecord> = window.to_vec();
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

pub(crate) fn duration_days(min_ts: i64, max_ts: i64) -> u32 {
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

pub(crate) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::git_history::FileChange;

    fn commit(subject: &str, added: u64, ts: i64) -> CommitRecord {
        CommitRecord {
            timestamp: ts,
            subject: subject.into(),
            files: vec![FileChange {
                path: "x.rs".into(),
                added,
                removed: 0,
            }],
        }
    }

    #[test]
    fn high_loc_and_templated_messages_raise_ai_score() {
        let c1 = commit("refactor: translate module foo", 3000, 1_700_000_000);
        let c2 = commit("refactor: port module bar", 2500, 1_700_086_400);
        let refs = [&c1, &c2];
        let score = compute_ai_assist_score(&refs, 5500, 5200);
        assert!(score >= 0.5, "expected high score, got {score}");
    }

    #[test]
    fn small_manual_commits_score_low() {
        let c1 = commit("fix typo", 3, 1_700_000_000);
        let c2 = commit("update readme", 10, 1_700_100_000);
        let refs = [&c1, &c2];
        let score = compute_ai_assist_score(&refs, 13, 6);
        assert!(score < 0.3, "expected low score, got {score}");
    }
}

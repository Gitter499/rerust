//! Detect massive language transitions by walking full commit history.
//!
//! One `git log --numstat` pass (oldest-first) maintains a running net line count
//! per language. We sample composition over time, find the steepest Rust rise,
//! and derive concrete rewrite metrics from the transition window.

use std::collections::HashMap;
use std::time::Duration;

use tracing::warn;

use super::enrich::{enrichment_from_totals, CommitEnrichment};
use super::git_history::{
    fetch_log, language_from_path, parse_numstat_log, CommitRecord, FileChange,
};

/// Outcome of analyzing commit history for language shifts.
#[derive(Debug, Clone, Default)]
pub struct HistoryAnalysis {
    /// Rewrite-window metrics suitable for storage / the site UI.
    pub enrichment: CommitEnrichment,
    /// Dominant non-Rust language in the early segment of history.
    pub from_language: Option<String>,
    /// Rust share (0–100) in the first 20% of analyzed commits.
    pub rust_pct_before: Option<f64>,
    /// Rust share (0–100) in the last 20% of analyzed commits.
    pub rust_pct_after: Option<f64>,
    /// Absolute swing in Rust share between before/after segments.
    pub transition_magnitude: Option<f64>,
    /// Total commits walked (after clone depth cap).
    pub total_commits: u32,
    /// True when we found a strong cross-language shift (not incidental Rust growth).
    pub strong_transition: bool,
}

/// Minimum Rust-share swing to call a transition "strong".
pub const MIN_TRANSITION_MAGNITUDE: f64 = 25.0;
/// Minimum late-history Rust share for a strong transition (rehydrate from store).
pub const MIN_RUST_AFTER_FOR_STRONG: f64 = 40.0;
/// Early/late segments each cover this fraction of commit history.
const SEGMENT_FRACTION: f64 = 0.20;
/// Minimum net code lines before composition ratios are meaningful.
const MIN_NET_LINES: u64 = 500;
/// Minimum share for a language to count as "dominant" in the early segment.
const MIN_DOMINANT_SHARE: f64 = 15.0;
/// Early-history Rust share must be below this to count as a migration (not
/// born-in-Rust ports like RuAnnoy/pounce).
pub const MAX_RUST_BEFORE_FOR_MIGRATION: f64 = 35.0;
/// Minimum rise in Rust share between early and late segments.
const MIN_RUST_RISE: f64 = 5.0;

/// Reconstruct `strong_transition` from persisted history fields.
pub fn strong_transition_from_stored(
    magnitude: Option<f64>,
    rust_before: Option<f64>,
    rust_after: Option<f64>,
    from_language: Option<&str>,
) -> bool {
    magnitude.unwrap_or(0.0) >= MIN_TRANSITION_MAGNITUDE
        && rust_after.unwrap_or(0.0) >= MIN_RUST_AFTER_FOR_STRONG
        && from_language.is_some()
        && rust_before.unwrap_or(100.0) <= MAX_RUST_BEFORE_FOR_MIGRATION
}

/// Options for commit-history analysis (timeouts, macro sampling).
#[derive(Debug, Clone, Copy)]
pub struct HistoryOptions {
    pub per_step_timeout: Duration,
    /// Wall-clock budget multiplier applied to `per_step_timeout`.
    pub budget_multiplier: u32,
    /// Use deeper clone strategies and denser composition sampling.
    pub macro_mode: bool,
    /// Max composition sample points (400 default, 1200 macro).
    pub sample_points: usize,
}

impl HistoryOptions {
    pub fn standard(per_step: Duration) -> Self {
        Self {
            per_step_timeout: per_step,
            budget_multiplier: 3,
            macro_mode: false,
            sample_points: 400,
        }
    }

    pub fn r#macro(per_step: Duration) -> Self {
        Self {
            per_step_timeout: per_step,
            budget_multiplier: 15,
            macro_mode: true,
            sample_points: 1200,
        }
    }

    fn repo_budget(&self) -> Duration {
        self.per_step_timeout.saturating_mul(self.budget_multiplier)
    }
}

/// Clone `repo_url`, walk history, and detect language transitions.
pub async fn analyze_history(repo_url: &str, opts: HistoryOptions) -> HistoryAnalysis {
    let tmp = match tempfile::Builder::new()
        .prefix("rerust-history-")
        .tempdir()
    {
        Ok(t) => t,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "history: could not create temp dir");
            return HistoryAnalysis::default();
        }
    };

    let repo_budget = opts.repo_budget();
    let log = match fetch_log(
        repo_url,
        tmp.path(),
        opts.per_step_timeout,
        repo_budget,
        opts.macro_mode,
    )
    .await
    {
        Some(s) => s,
        None => return HistoryAnalysis::default(),
    };

    let commits = parse_numstat_log(&log);
    analyze_commits_with_sample_points(&commits, opts.sample_points)
}

/// Core analysis over already-parsed commits (unit-testable without git).
#[cfg(test)]
fn analyze_commits(commits: &[CommitRecord]) -> HistoryAnalysis {
    analyze_commits_with_sample_points(commits, 400)
}

pub fn analyze_commits_with_sample_points(
    commits: &[CommitRecord],
    sample_points: usize,
) -> HistoryAnalysis {
    if commits.is_empty() {
        return HistoryAnalysis::default();
    }

    let total_commits = commits.len() as u32;
    let samples = sample_composition(commits, sample_points);
    if samples.is_empty() {
        return HistoryAnalysis {
            total_commits,
            ..Default::default()
        };
    }

    let early_end = ((samples.len() as f64) * SEGMENT_FRACTION).ceil() as usize;
    let late_start = samples.len().saturating_sub(early_end.max(1));
    let early = average_sample(&samples[..early_end.max(1)]);
    let late = average_sample(&samples[late_start..]);

    let rust_before = pct_for_lang(&early, "Rust");
    let rust_after = pct_for_lang(&late, "Rust");
    let rising = rust_after > rust_before + MIN_RUST_RISE;
    let magnitude = if rising {
        rust_after - rust_before
    } else {
        0.0
    };

    let from_language = if rising
        && magnitude >= 10.0
        && rust_before <= MAX_RUST_BEFORE_FOR_MIGRATION
    {
        dominant_non_rust(&early)
            .filter(|lang| !lang.eq_ignore_ascii_case("rust"))
            .filter(|lang| crate::detect::commits::is_real_application_language(lang))
    } else {
        None
    };

    let late_sample = samples.last();
    let late_net_lines = late_sample.map(|s| net_total(&s.net)).unwrap_or(0);

    let strong = rising
        && magnitude >= MIN_TRANSITION_MAGNITUDE
        && rust_before <= MAX_RUST_BEFORE_FOR_MIGRATION
        && rust_after >= 40.0
        && from_language.is_some()
        && late_net_lines >= MIN_NET_LINES;

    let window = transition_window_commits(commits, &samples);
    let enrichment = enrichment_from_window(&window);

    HistoryAnalysis {
        enrichment,
        from_language,
        rust_pct_before: Some(round1(rust_before)),
        rust_pct_after: Some(round1(rust_after)),
        transition_magnitude: if rising {
            Some(round1(magnitude))
        } else {
            None
        },
        total_commits,
        strong_transition: strong,
    }
}

/// Net line counts per language after walking commits `0..=idx`.
#[derive(Debug, Clone, Default)]
struct CompositionSample {
    idx: usize,
    net: HashMap<String, i64>,
}

fn sample_composition(commits: &[CommitRecord], max_points: usize) -> Vec<CompositionSample> {
    let mut net: HashMap<String, i64> = HashMap::new();
    let mut samples = Vec::with_capacity(commits.len());

    // Sample every commit for small repos; subsample for large ones.
    let cap = max_points.max(100);
    let step = (commits.len() / cap).max(1);

    for (idx, commit) in commits.iter().enumerate() {
        apply_commit(&mut net, commit);
        if idx + 1 == commits.len() || idx % step == 0 {
            samples.push(CompositionSample {
                idx,
                net: net.clone(),
            });
        }
    }
    samples
}

fn apply_commit(net: &mut HashMap<String, i64>, commit: &CommitRecord) {
    for f in &commit.files {
        apply_file(net, f);
    }
}

fn apply_file(net: &mut HashMap<String, i64>, f: &FileChange) {
    let Some(lang) = language_from_path(&f.path) else {
        return;
    };
    let delta = f.added as i64 - f.removed as i64;
    if delta == 0 {
        return;
    }
    let entry = net.entry(lang.to_string()).or_insert(0);
    *entry += delta;
    if *entry < 0 {
        *entry = 0;
    }
}

fn average_sample(samples: &[CompositionSample]) -> HashMap<String, f64> {
    if samples.is_empty() {
        return HashMap::new();
    }
    let mut sums: HashMap<String, f64> = HashMap::new();
    for s in samples {
        let total = net_total_f64(&s.net);
        if total <= 0.0 {
            continue;
        }
        for (lang, lines) in &s.net {
            if *lines <= 0 {
                continue;
            }
            *sums.entry(lang.clone()).or_insert(0.0) += (*lines as f64 / total) * 100.0;
        }
    }
    let n = samples.len() as f64;
    sums.into_iter().map(|(k, v)| (k, v / n)).collect()
}

fn pct_for_lang(comp: &HashMap<String, f64>, lang: &str) -> f64 {
    comp.get(lang).copied().unwrap_or(0.0)
}

fn dominant_non_rust(comp: &HashMap<String, f64>) -> Option<String> {
    comp.iter()
        .filter(|(l, share)| !l.eq_ignore_ascii_case("rust") && **share >= MIN_DOMINANT_SHARE)
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
}

fn net_total(net: &HashMap<String, i64>) -> u64 {
    net.values().filter(|v| **v > 0).map(|v| *v as u64).sum()
}

fn net_total_f64(net: &HashMap<String, i64>) -> f64 {
    net.values().filter(|v| **v > 0).map(|v| *v as f64).sum()
}

/// Commits between the steepest rise in Rust share (25% → 75% crossings).
fn transition_window_commits<'a>(
    commits: &'a [CommitRecord],
    samples: &'a [CompositionSample],
) -> Vec<&'a CommitRecord> {
    if commits.is_empty() || samples.len() < 2 {
        return Vec::new();
    }

    let mut best_slope = 0.0f64;
    let mut best_range = (0usize, commits.len().saturating_sub(1));

    for w in samples.windows(2) {
        let a = &w[0];
        let b = &w[1];
        let rust_a = rust_share(&a.net);
        let rust_b = rust_share(&b.net);
        let slope = rust_b - rust_a;
        if slope > best_slope {
            best_slope = slope;
            best_range = (a.idx, b.idx);
        }
    }

    // Expand window: from first sample with rust < 25% before peak to last > 50% after.
    let mut start = best_range.0;
    let mut end = best_range.1;
    for s in samples.iter() {
        if s.idx <= best_range.0 && rust_share(&s.net) < 25.0 {
            start = start.min(s.idx);
        }
        if s.idx >= best_range.1 && rust_share(&s.net) > 50.0 {
            end = end.max(s.idx);
        }
    }

    if best_slope < 5.0 {
        // No clear slope: use commits with heavy Rust adds + non-Rust removals.
        return commits
            .iter()
            .filter(|c| transition_churn(c) > 0)
            .collect();
    }

    commits[start..=end.min(commits.len() - 1)].iter().collect()
}

fn rust_share(net: &HashMap<String, i64>) -> f64 {
    let total = net_total_f64(net);
    if total <= 0.0 {
        return 0.0;
    }
    let rust = *net.get("Rust").unwrap_or(&0).max(&0) as f64;
    (rust / total) * 100.0
}

fn transition_churn(c: &CommitRecord) -> u64 {
    let mut rust_added = 0u64;
    let mut other_removed = 0u64;
    for f in &c.files {
        let Some(lang) = language_from_path(&f.path) else {
            continue;
        };
        if lang == "Rust" {
            rust_added += f.added;
        } else {
            other_removed += f.removed;
        }
    }
    rust_added.saturating_add(other_removed)
}

fn enrichment_from_window(window: &[&CommitRecord]) -> CommitEnrichment {
    if window.is_empty() {
        return CommitEnrichment::default();
    }

    let lines_added: u64 = window
        .iter()
        .flat_map(|c| c.files.iter())
        .map(|f| f.added)
        .sum();
    let lines_removed: u64 = window
        .iter()
        .flat_map(|c| c.files.iter())
        .map(|f| f.removed)
        .sum();
    let min_ts = window.iter().map(|c| c.timestamp).min().unwrap_or(0);
    let max_ts = window.iter().map(|c| c.timestamp).max().unwrap_or(0);

    enrichment_from_totals(
        lines_added,
        lines_removed,
        window.len() as u32,
        min_ts,
        max_ts,
        window,
    )
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::git_history::FileChange;

    fn commit(ts: i64, files: Vec<(&str, u64, u64)>) -> CommitRecord {
        CommitRecord {
            timestamp: ts,
            subject: "work".into(),
            files: files
                .into_iter()
                .map(|(p, a, r)| FileChange {
                    path: p.into(),
                    added: a,
                    removed: r,
                })
                .collect(),
        }
    }

    #[test]
    fn born_in_rust_port_shows_no_transition() {
        // Simulates RuAnnoy/pounce: always mostly Rust, no rising migration.
        let commits = vec![
            commit(1_000, vec![("lib.rs", 5000, 0), ("bind.cs", 1000, 0)]),
            commit(2_000, vec![("lib.rs", 2000, 0), ("bind.cs", 500, 0)]),
            commit(3_000, vec![("lib.rs", 1500, 0)]),
        ];
        let h = analyze_commits(&commits);
        assert!(!h.strong_transition);
        assert!(h.from_language.is_none());
        assert!(h.transition_magnitude.is_none());
    }

    #[test]
    fn detects_python_to_rust_transition() {
        let commits = vec![
            commit(1_000, vec![("main.py", 5000, 0), ("util.py", 3000, 0)]),
            commit(2_000, vec![("main.py", 0, 2000), ("main.rs", 2500, 0)]),
            commit(3_000, vec![("main.py", 0, 3000), ("main.rs", 3000, 0)]),
            commit(4_000, vec![("util.py", 0, 3000), ("util.rs", 2800, 0)]),
            commit(5_000, vec![("main.rs", 500, 0), ("lib.rs", 2000, 0)]),
        ];
        let h = analyze_commits(&commits);
        assert!(h.rust_pct_after.unwrap() > h.rust_pct_before.unwrap());
        assert!(h.transition_magnitude.unwrap() >= 20.0);
        assert_eq!(h.from_language.as_deref(), Some("Python"));
        assert!(h.strong_transition);
        assert!(h.enrichment.lines_added.unwrap() > 0);
    }

    #[test]
    fn all_rust_from_start_has_no_transition() {
        let commits = vec![
            commit(1_000, vec![("a.rs", 1000, 0)]),
            commit(2_000, vec![("b.rs", 500, 0)]),
        ];
        let h = analyze_commits(&commits);
        assert!(!h.strong_transition);
        assert!(h.from_language.is_none());
    }

    #[test]
    fn c_to_rust_coreutils_style() {
        let commits = vec![
            commit(1_000, vec![("cp.c", 4000, 0), ("mv.c", 3500, 0)]),
            commit(2_000, vec![("cp.c", 0, 500), ("cp.rs", 600, 0)]),
            commit(3_000, vec![("mv.c", 0, 400), ("mv.rs", 550, 0)]),
            commit(4_000, vec![("cp.rs", 800, 0), ("mv.rs", 700, 0)]),
            commit(5_000, vec![("cp.c", 0, 3500), ("mv.c", 0, 3100)]),
            commit(6_000, vec![("lib.rs", 5000, 0)]),
        ];
        let h = analyze_commits(&commits);
        assert_eq!(h.from_language.as_deref(), Some("C"));
        assert!(h.rust_pct_after.unwrap() > 60.0);
    }
}

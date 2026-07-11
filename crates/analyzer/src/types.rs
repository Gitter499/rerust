//! Core data types shared across the detection pipeline.

use serde::{Deserialize, Serialize};

/// A signal is a single piece of evidence suggesting a Rust rewrite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    /// Where the signal came from, e.g. "repo-search", "issue", "pull-request".
    pub kind: String,
    /// Short human-readable description of the matched evidence.
    pub detail: String,
    /// A link to the source of the signal (repo, issue, PR, etc.).
    pub url: String,
}

/// A candidate repository discovered during the scan, before scoring.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    /// "owner/name" full name.
    pub full_name: String,
    pub html_url: String,
    pub description: Option<String>,
    pub stars: u64,
    /// Number of forks reported by the repo endpoint.
    pub forks: u64,
    /// Open issues (pull requests excluded), derived during enrichment.
    pub open_issues: u64,
    /// Open pull requests, fetched via the search API.
    pub open_prs: u64,
    /// Byte breakdown per language from the GitHub languages endpoint.
    pub languages: Vec<(String, u64)>,
    pub created_at: Option<String>,
    pub pushed_at: Option<String>,
    /// Accumulated evidence for this candidate.
    pub signals: Vec<Signal>,
    /// Share of unsafe Rust (0.0 - 100.0) measured by cargo-geiger, when the
    /// opt-in `--measure-unsafe` scan runs. `None` when not measured.
    pub unsafe_percentage: Option<f64>,
    /// The specific prior project this repo displaces or reimplements, parsed
    /// from migration/competitor phrasing ("port of X", "alternative to X",
    /// "drop-in replacement for X"). Drives structural-provenance
    /// classification. `None` when no named predecessor is detected.
    pub named_origin: Option<String>,
}

/// The pull request that best represents the actual rewrite work, surfaced as
/// the marquee piece of evidence on a project card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewritePr {
    pub title: String,
    pub url: String,
}

/// A fully scored project ready to be stored and rendered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub repo_url: String,
    pub description: Option<String>,
    pub stars: u64,
    /// Number of forks reported by GitHub.
    #[serde(default)]
    pub forks: u64,
    /// Open issues (pull requests excluded).
    #[serde(default)]
    pub open_issues: u64,
    /// Open pull requests.
    #[serde(default)]
    pub open_prs: u64,
    /// Largest non-Rust language, inferred as the displaced/original language.
    pub original_language: Option<String>,
    /// Rust share of the codebase, 0.0 - 100.0.
    pub rust_percentage: f64,
    /// Composite confidence that this is a genuine Rust rewrite, 0.0 - 1.0.
    pub confidence: f64,
    pub signals: Vec<Signal>,
    /// The pull request that most likely performed the rewrite, if detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_pr: Option<RewritePr>,
    /// Share of unsafe Rust (0.0 - 100.0), measured by cargo-geiger during an
    /// opt-in `--measure-unsafe` scan. `None` when unmeasured or unavailable.
    #[serde(default)]
    pub unsafe_percentage: Option<f64>,
    /// How this project relates to its predecessor under structural provenance:
    /// `"rewrite"` (same shipping product migrated to Rust),
    /// `"replacement"` (new Rust tool competing with an external one), or
    /// `"neither"` (no real cross-language provenance — filtered out).
    /// See [`crate::detect::classify`].
    #[serde(default = "default_project_kind")]
    pub project_kind: String,
    /// The specific prior project this repo displaces or reimplements, when
    /// detected (e.g. "yjs", "gnu coreutils", "grep"). Surfaced for reporting
    /// and used by the provenance classifier. `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_origin: Option<String>,
    /// Total lines added during the rewrite window (`--enrich-commits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    /// Total lines removed during the rewrite window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    /// Lines modified per day during the active rewrite window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_velocity: Option<f64>,
    /// Experimental 0.0–1.0 heuristic for signs of AI-assisted translation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_assist_score: Option<f64>,
    /// Days from first to last rewrite-signal commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_duration_days: Option<u32>,
    /// Commits in the rewrite window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_count: Option<u32>,
    /// Dominant non-Rust language detected from commit history (early segment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_from_language: Option<String>,
    /// Rust share in the first ~20% of analyzed commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_rust_before: Option<f64>,
    /// Rust share in the last ~20% of analyzed commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_rust_after: Option<f64>,
    /// Swing in Rust share (after − before) from commit history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_magnitude: Option<f64>,
    /// Total commits walked during history analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_commits_analyzed: Option<u32>,
    /// Backfill outcome: `ok`, `failed`, `skipped_huge`, or unset (`None` = pending).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<String>,
    /// Last backfill error message (truncated), when `history_status` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_error: Option<String>,
    /// ISO-8601 timestamp of the last history backfill attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_attempted_at: Option<String>,
    /// Number of failed/empty history attempts (resets on success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_attempts: Option<u32>,
    /// Primary source link that best explains the detection.
    pub source_url: String,
    /// ISO-8601 timestamp of first detection (set on insert).
    pub first_detected: String,
    /// ISO-8601 timestamp of the most recent scan that saw this project.
    pub last_seen: String,
    /// Curated exemplar rewrite — pinned to top of site when true.
    #[serde(default)]
    pub exemplar: bool,
}

/// Default `project_kind` for rows/payloads that predate the field. Defaults to
/// the safer, non-over-claiming `"replacement"` under the current taxonomy.
fn default_project_kind() -> String {
    "replacement".to_string()
}

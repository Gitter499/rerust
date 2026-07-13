//! Core data types shared across the detection pipeline.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    /// e.g. "repo-search", "issue", "pull-request".
    pub kind: String,
    pub detail: String,
    pub url: String,
}

/// Candidate repository before scoring.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    /// "owner/name".
    pub full_name: String,
    pub html_url: String,
    pub description: Option<String>,
    pub stars: u64,
    pub forks: u64,
    /// Open issues with PRs excluded.
    pub open_issues: u64,
    pub open_prs: u64,
    pub languages: Vec<(String, u64)>,
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewritePr {
    pub title: String,
    pub url: String,
}

/// Scored project ready for storage and rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub repo_url: String,
    pub description: Option<String>,
    pub stars: u64,
    #[serde(default)]
    pub forks: u64,
    #[serde(default)]
    pub open_issues: u64,
    #[serde(default)]
    pub open_prs: u64,
    pub original_language: Option<String>,
    pub rust_percentage: f64,
    pub confidence: f64,
    pub signals: Vec<Signal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewrite_prs: Vec<RewritePr>,
    /// Primary rewrite PR (first of `rewrite_prs`) for older consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_pr: Option<RewritePr>,
    #[serde(default)]
    pub unsafe_percentage: Option<f64>,
    /// `"rewrite"`, `"replacement"`, or `"neither"`.
    #[serde(default = "default_project_kind")]
    pub project_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_velocity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_assist_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_duration_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_from_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_rust_before: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_rust_after: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_magnitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_commits_analyzed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_attempted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_attempts: Option<u32>,
    pub source_url: String,
    pub first_detected: String,
    pub last_seen: String,
    #[serde(default)]
    pub exemplar: bool,
}

impl Project {
    /// Prefer `rewrite_prs`; fall back to legacy single `rewrite_pr`.
    pub fn effective_rewrite_prs(&self) -> Vec<RewritePr> {
        if self.rewrite_prs.is_empty() {
            self.rewrite_pr.clone().into_iter().collect()
        } else {
            self.rewrite_prs.clone()
        }
    }
}

fn default_project_kind() -> String {
    "replacement".to_string()
}

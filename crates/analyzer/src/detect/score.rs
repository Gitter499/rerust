//! Composite confidence scoring.
//!
//! Confidence combines two independent lines of evidence:
//!   * textual/heuristic signals (what people *say* about the project), and
//!   * language composition (what the repo *actually* contains).
//!
//! Neither alone is decisive: a repo can talk about a rewrite that never
//! happened, or be mostly Rust for unrelated reasons. Requiring both to line up
//! produces a far more trustworthy score. Weights are intentionally simple and
//! easy to tune; a learned classifier is a planned future addition.

use crate::detect::classify::{
    classify, has_strong_rewrite_signal, named_origin, signal_title_body, ProjectKind,
};
use crate::detect::commits::LanguageAnalysis;
use crate::detect::enrich::{round2, CommitEnrichment};
use crate::detect::transitions::HistoryAnalysis;
use crate::types::{Candidate, Project, RewritePr};

/// Default scan threshold in `main.rs`; used by tests to assert filtering.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.15;

/// When there is no substantial displaced language, confidence is capped here
/// unless the repo explicitly advertises replacing an existing tool.
const NO_DISPLACED_CAP: f64 = 0.12;

/// Confidence cap for projects that lack displaced-language bytes but carry
/// strong replacement/migration wording (e.g. ripgrep, or an already-100%-Rust
/// rewrite).
const REPLACEMENT_REWRITE_CAP: f64 = 0.55;

/// Small confidence edge granted to genuine **Rewrites** (a project's own code
/// migrated to Rust), which are the tool's primary focus. It is deliberately
/// modest — a nudge that surfaces true rewrites above equivalent replacements
/// rather than fabricated certainty — and is applied *before* the
/// no-displaced-language cap, so a rewrite without displaced bytes still can't
/// exceed [`REPLACEMENT_REWRITE_CAP`].
const REWRITE_CONFIDENCE_BONUS: f64 = 0.05;

/// Per-signal contribution to the textual evidence score.
fn signal_weight(kind: &str) -> f64 {
    match kind {
        // The repo itself advertising a rewrite is the strongest text signal.
        "repo-search" => 0.35,
        // Curated exemplar from data/exemplars.txt.
        "exemplar" => 0.40,
        // An open/merged PR about a rewrite is strong corroboration.
        "pull-request" => 0.20,
        // A mere issue is weaker (could be a wishlist item).
        "issue" => 0.10,
        _ => 0.05,
    }
}

/// Maximum share of the total score attributable to text signals alone.
const TEXT_CAP: f64 = 0.60;
/// Maximum share attributable to language composition alone.
const LANG_CAP: f64 = 0.50;

/// Maximum share attributable to commit-history transition evidence.
const HISTORY_CAP: f64 = 0.35;

/// Combine a candidate and its language analysis into a scored [`Project`].
pub fn score(
    candidate: &Candidate,
    analysis: &LanguageAnalysis,
    enrichment: &CommitEnrichment,
    history: &HistoryAnalysis,
    now_iso: &str,
) -> Project {
    let text_evidence: f64 = candidate
        .signals
        .iter()
        .map(|s| signal_weight(&s.kind))
        .sum::<f64>()
        .min(TEXT_CAP);

    // Language evidence scales with Rust's share when a displaced language is
    // present; a Rust-heavy repo alone does not prove a rewrite happened.
    let has_displaced = analysis.original_language.is_some();
    let mut lang_evidence = if has_displaced {
        (analysis.rust_percentage / 100.0) * 0.4
    } else {
        (analysis.rust_percentage / 100.0) * 0.1
    };
    if analysis.rust_is_primary {
        lang_evidence += if has_displaced { 0.15 } else { 0.05 };
    }
    let lang_evidence = lang_evidence.min(LANG_CAP);

    // Commit-history transition: only credit *rising* Rust share (real migration).
    let mut history_evidence = 0.0f64;
    if history.strong_transition {
        history_evidence += 0.20;
        if let Some(mag) = history.transition_magnitude {
            history_evidence += (mag / 100.0 * 0.25).min(0.15);
        }
        if history.from_language.is_some() {
            history_evidence += 0.05;
        }
    }
    let history_evidence = history_evidence.min(HISTORY_CAP);

    let kind = classify(candidate, analysis, history);

    let mut confidence =
        (text_evidence + lang_evidence + history_evidence).clamp(0.0, 1.0);

    // Rewrites are the headline category: nudge them slightly ahead of otherwise
    // comparable replacements. Applied before the cap below so a no-displaced
    // rewrite still can't exceed REPLACEMENT_REWRITE_CAP.
    if kind == ProjectKind::Rewrite {
        confidence = (confidence + REWRITE_CONFIDENCE_BONUS).min(1.0);
    }

    // Without a real displaced language, only strong replacement/migration
    // signals justify surfacing the project; incidental "rewrite in Rust"
    // discovery hits (e.g. helix) are capped below the default min-confidence
    // filter.
    if !has_displaced {
        let cap = if has_strong_rewrite_signal(candidate) {
            REPLACEMENT_REWRITE_CAP
        } else {
            NO_DISPLACED_CAP
        };
        confidence = confidence.min(cap);
    }

    // Projects with no genuine cross-language provenance are dropped: force
    // confidence to zero so the min-confidence filter (scan) skips storing them
    // and reclassify deletes them, keeping the site free of native-Rust noise
    // and Rust-crate compatibility shims.
    if kind == ProjectKind::Neither {
        confidence = 0.0;
    }

    let original_language = sanitize_original_language(
        analysis
            .original_language
            .clone()
            .or_else(|| {
                if history.strong_transition {
                    history.from_language.clone()
                } else {
                    None
                }
            }),
    );

    let name = candidate
        .full_name
        .rsplit('/')
        .next()
        .unwrap_or(&candidate.full_name)
        .to_string();

    let repo_url = if candidate.html_url.is_empty() {
        format!("https://github.com/{}", candidate.full_name)
    } else {
        candidate.html_url.clone()
    };

    Project {
        name,
        repo_url: repo_url.clone(),
        description: candidate.description.clone(),
        stars: candidate.stars,
        forks: candidate.forks,
        open_issues: candidate.open_issues,
        open_prs: candidate.open_prs,
        original_language,
        rust_percentage: round2(analysis.rust_percentage),
        confidence: round2(confidence),
        rewrite_pr: select_rewrite_pr(candidate),
        unsafe_percentage: None,
        project_kind: kind.as_str().to_string(),
        named_origin: named_origin(candidate),
        lines_added: enrichment.lines_added,
        lines_removed: enrichment.lines_removed,
        rewrite_velocity: enrichment.rewrite_velocity,
        ai_assist_score: enrichment.ai_assist_score,
        rewrite_duration_days: enrichment.rewrite_duration_days,
        commit_count: enrichment.commit_count,
        history_from_language: if history.strong_transition {
            history.from_language.clone()
        } else {
            None
        },
        history_rust_before: if history.strong_transition {
            history.rust_pct_before
        } else {
            None
        },
        history_rust_after: if history.strong_transition {
            history.rust_pct_after
        } else {
            None
        },
        transition_magnitude: history.transition_magnitude,
        total_commits_analyzed: if history.total_commits > 0 {
            Some(history.total_commits)
        } else {
            None
        },
        history_status: None,
        history_error: None,
        history_attempted_at: None,
        history_attempts: None,
        source_url: primary_source(candidate).unwrap_or(repo_url),
        signals: candidate.signals.clone(),
        first_detected: now_iso.to_string(),
        last_seen: now_iso.to_string(),
        exemplar: false,
    }
}

/// Pick the pull-request signal that best represents the rewrite.
///
/// PR signals are ranked by how strongly their title implies a rewrite
/// ("rewrite" > "port" > "rust"); the highest-scoring one wins, falling back to
/// the first PR signal when none of the keywords match. Uses only signal data
/// already collected during discovery — no extra API calls.
fn select_rewrite_pr(candidate: &Candidate) -> Option<RewritePr> {
    candidate
        .signals
        .iter()
        .filter(|s| s.kind == "pull-request")
        // Reverse so that, on equal relevance, `max_by_key` yields the first PR.
        .rev()
        .max_by_key(|s| pr_relevance(&signal_title(&s.detail)))
        .map(|s| RewritePr {
            title: signal_title(&s.detail),
            url: s.url.clone(),
        })
}

/// Keyword-based relevance for a PR title indicating it performed the rewrite.
fn pr_relevance(title: &str) -> i32 {
    let t = title.to_lowercase();
    let mut score = 0;
    if t.contains("rewrite") {
        score += 3;
    }
    if t.contains("port") {
        score += 2;
    }
    if t.contains("rust") {
        score += 1;
    }
    score
}

/// Recover the raw issue/PR title from a signal `detail` of the form
/// `"<label>: <title>"`, falling back to the whole detail if unsplittable.
fn signal_title(detail: &str) -> String {
    signal_title_body(detail).to_string()
}

/// Choose the most explanatory source link for the detection.
fn primary_source(candidate: &Candidate) -> Option<String> {
    for kind in ["repo-search", "pull-request", "issue"] {
        if let Some(sig) = candidate.signals.iter().find(|s| s.kind == kind) {
            return Some(sig.url.clone());
        }
    }
    candidate.signals.first().map(|s| s.url.clone())
}

/// Never emit Rust or noise languages as the displaced original.
fn sanitize_original_language(lang: Option<String>) -> Option<String> {
    lang.filter(|l| !l.eq_ignore_ascii_case("rust"))
        .filter(|l| crate::detect::commits::is_real_application_language(l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::commits;
    use crate::types::Signal;

    fn helix_like_candidate() -> Candidate {
        Candidate {
            full_name: "helix-editor/helix".into(),
            html_url: "https://github.com/helix-editor/helix".into(),
            description: Some("A post-modern modal text editor.".into()),
            languages: vec![("Rust".into(), 980_000), ("Nix".into(), 20_000)],
            signals: vec![Signal {
                kind: "repo-search".into(),
                detail: "repo mentions \"rewrite in Rust\"".into(),
                url: "https://github.com/helix-editor/helix".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn helix_like_filtered_below_default_min_confidence() {
        let candidate = helix_like_candidate();
        let analysis = commits::analyze(&candidate);
        assert!(analysis.original_language.is_none());

        let project = score(
            &candidate,
            &analysis,
            &CommitEnrichment::default(),
            &HistoryAnalysis::default(),
            "2026-01-01T00:00:00Z",
        );

        assert!(project.original_language.is_none());
        assert_ne!(project.original_language.as_deref(), Some("Rust"));
        // The guardrail: an all-Rust editor with no displaced language and only a
        // bare "rewrite in Rust" discovery hit stays below the min-confidence
        // filter, so it never surfaces as a bogus rewrite regardless of label.
        assert!(
            project.confidence < DEFAULT_MIN_CONFIDENCE,
            "expected confidence below {}, got {}",
            DEFAULT_MIN_CONFIDENCE,
            project.confidence
        );
    }

    #[test]
    fn coreutils_like_rewrite_scores_high() {
        let candidate = Candidate {
            full_name: "uutils/coreutils".into(),
            description: Some("Cross-platform Rust rewrite of the GNU coreutils".into()),
            languages: vec![("Rust".into(), 600_000), ("C".into(), 400_000)],
            signals: vec![Signal {
                kind: "repo-search".into(),
                detail: "repo describes a port to Rust".into(),
                url: "https://github.com/uutils/coreutils".into(),
            }],
            ..Default::default()
        };
        let analysis = commits::analyze(&candidate);
        assert_eq!(analysis.original_language.as_deref(), Some("C"));

        let project = score(
            &candidate,
            &analysis,
            &CommitEnrichment::default(),
            &HistoryAnalysis::default(),
            "2026-01-01T00:00:00Z",
        );

        // Without commit history, README + displaced-language snapshot is not
        // enough for Rewrite under the commit-analysis gate.
        assert_eq!(project.project_kind, "replacement");
        assert_eq!(project.original_language.as_deref(), Some("C"));
        assert!(project.confidence >= DEFAULT_MIN_CONFIDENCE);
    }

    #[test]
    fn ripgrep_style_replacement_without_displaced_language_can_score_high() {
        let candidate = Candidate {
            description: Some(
                "recursively searches directories for a regex pattern, a faster alternative to grep"
                    .into(),
            ),
            languages: vec![("Rust".into(), 980_000), ("C".into(), 20_000)],
            signals: vec![Signal {
                kind: "repo-search".into(),
                detail: "repo is a drop-in replacement written in Rust".into(),
                url: "https://example".into(),
            }],
            ..Default::default()
        };
        let analysis = commits::analyze(&candidate);
        assert!(analysis.original_language.is_none());

        let project = score(
            &candidate,
            &analysis,
            &CommitEnrichment::default(),
            &HistoryAnalysis::default(),
            "2026-01-01T00:00:00Z",
        );

        assert_eq!(project.project_kind, "replacement");
        assert!(project.confidence >= DEFAULT_MIN_CONFIDENCE);
    }

    #[test]
    fn sanitize_never_allows_rust_as_original_language() {
        assert_eq!(sanitize_original_language(Some("Rust".into())), None);
        assert_eq!(sanitize_original_language(Some("rust".into())), None);
        assert_eq!(sanitize_original_language(Some("C".into())).as_deref(), Some("C"));
    }
}

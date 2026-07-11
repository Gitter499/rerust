//! Candidate discovery via GitHub search heuristics.
//!
//! Two families of searches feed the candidate set:
//!
//!   * **Repository searches** — Rust (or mid-migration) repos whose
//!     name/description/README advertise a cross-language migration.
//!   * **Issue/PR searches** — discussions about rewriting a project in Rust.
//!     The *owning* repository becomes the candidate even when it is not yet
//!     primarily Rust. This is how same-product migrations (Astro compiler,
//!     React compiler, …) surface naturally: the signal lives on the original
//!     project, not on a third-party "Rust port of X" marketing page.
//!
//! Each match contributes a [`Signal`] to a per-repository [`Candidate`].
//! Deduplicated by "owner/name". Classification (identity continuity) decides
//! Rewrite vs Replacement; discovery stays wide on purpose.

use std::collections::HashMap;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::github::GitHub;
use crate::types::{Candidate, Signal};

/// A search phrase paired with the human-readable reason it signals a rewrite.
struct Query {
    /// The raw GitHub search query string.
    q: &'static str,
    /// Human-readable description stored on the resulting signal.
    label: &'static str,
}

/// Repository-targeted queries.
///
/// Prefer identity-continuity / migration phrasing. Keep `language:Rust` for
/// finished migrations, but also run a few **language-agnostic** README/desc
/// queries so mid-migration repos (still mostly Go/TS/…) are not invisible.
/// Replacement-oriented queries stay in the net; the classifier demotes them.
const REPO_QUERIES: &[Query] = &[
    // --- Finished Rust migrations (identity-friendly phrasing) ---
    Query {
        q: "\"rewritten in rust\" language:Rust",
        label: "repo mentions \"rewritten in Rust\"",
    },
    Query {
        q: "\"rewrite in rust\" language:Rust",
        label: "repo mentions \"rewrite in Rust\"",
    },
    Query {
        q: "\"rewriting in rust\" language:Rust",
        label: "repo mentions \"rewriting in Rust\"",
    },
    Query {
        q: "\"migrated to rust\" language:Rust",
        label: "repo mentions \"migrated to Rust\"",
    },
    Query {
        q: "\"migrating to rust\" language:Rust",
        label: "repo mentions \"migrating to Rust\"",
    },
    Query {
        q: "\"rewrite of\" language:Rust",
        label: "repo describes a rewrite of a named project",
    },
    Query {
        q: "\"reimplementation of\" rust language:Rust",
        label: "repo describes a reimplementation of a named project",
    },
    Query {
        q: "\"reimplementation in rust\" language:Rust",
        label: "repo mentions \"reimplementation in Rust\"",
    },
    // Cross-language "from X to Rust" — split to stay under GitHub's 5-operator limit.
    Query {
        q: "\"from go\" \"to rust\" language:Rust",
        label: "repo describes a from-Go-to-Rust migration",
    },
    Query {
        q: "\"from typescript\" \"to rust\" language:Rust",
        label: "repo describes a from-TypeScript-to-Rust migration",
    },
    Query {
        q: "\"from javascript\" \"to rust\" language:Rust",
        label: "repo describes a from-JavaScript-to-Rust migration",
    },
    Query {
        q: "\"from python\" \"to rust\" language:Rust",
        label: "repo describes a from-Python-to-Rust migration",
    },
    Query {
        q: "\"from c++\" \"to rust\" language:Rust",
        label: "repo describes a from-C++-to-Rust migration",
    },
    Query {
        q: "\"from java\" \"to rust\" language:Rust",
        label: "repo describes a from-Java-to-Rust migration",
    },
    Query {
        q: "\"port of\" \"in rust\" language:Rust",
        label: "repo describes a port to Rust",
    },
    Query {
        q: "\"rust port of\" language:Rust",
        label: "repo mentions a \"Rust port of\" a named project",
    },
    Query {
        q: "oxidize \"rewrite\" language:Rust",
        label: "repo references oxidizing/porting to Rust",
    },
    // Replacement net (classifier → Replacement / Neither).
    Query {
        q: "\"drop-in replacement\" language:Rust",
        label: "repo is a drop-in replacement written in Rust",
    },
    Query {
        q: "\"alternative to\" language:Rust stars:>50",
        label: "repo positions itself as an alternative (Rust)",
    },
    // --- Mid-migration / README (no language:Rust) ---
    // Catches original products still mixed-language whose README announces
    // the Rust rewrite. Sorted by stars so popular exemplars win the page budget.
    Query {
        q: "\"rewritten in rust\" in:readme,description stars:>100",
        label: "README/description mentions \"rewritten in Rust\"",
    },
    Query {
        q: "\"rewriting in rust\" in:readme,description stars:>50",
        label: "README/description mentions \"rewriting in Rust\"",
    },
    Query {
        q: "\"migrating to rust\" in:readme,description stars:>50",
        label: "README/description mentions \"migrating to Rust\"",
    },
    Query {
        q: "\"porting to rust\" in:readme,description stars:>50",
        label: "README/description mentions \"porting to Rust\"",
    },
];

/// Issue/PR-targeted queries. The owning repository becomes a candidate even
/// if it is not (yet) primarily Rust — the main path for discovering official
/// same-product migrations in progress or recently completed.
///
/// Every query carries `is:issue` / `is:pull-request` (GitHub 422 otherwise).
const ISSUE_QUERIES: &[Query] = &[
    // Issues: wishlist / tracking on the original product.
    Query {
        q: "\"rewrite in rust\" in:title is:issue",
        label: "issue titled about rewriting in Rust",
    },
    Query {
        q: "\"rewrite it in rust\" in:title is:issue",
        label: "issue titled \"rewrite it in Rust\"",
    },
    Query {
        q: "\"rewriting in rust\" in:title is:issue",
        label: "issue titled about rewriting in Rust",
    },
    Query {
        q: "\"port to rust\" in:title is:issue",
        label: "issue titled about porting to Rust",
    },
    Query {
        q: "\"migrate to rust\" in:title is:issue",
        label: "issue titled about migrating to Rust",
    },
    // PRs: actual migration work on the original product.
    Query {
        q: "\"rewrite in rust\" in:title is:pull-request",
        label: "PR titled about rewriting in Rust",
    },
    Query {
        q: "\"rewriting in rust\" in:title is:pull-request",
        label: "PR about rewriting in Rust",
    },
    Query {
        q: "\"port to rust\" in:title is:pull-request",
        label: "PR titled about porting to Rust",
    },
    Query {
        q: "\"ported to rust\" in:title is:pull-request",
        label: "PR titled about a port to Rust",
    },
    Query {
        q: "\"migrate to rust\" in:title is:pull-request",
        label: "PR titled about migrating to Rust",
    },
    Query {
        q: "\"migrated to rust\" in:title is:pull-request",
        label: "PR titled about a migration to Rust",
    },
    Query {
        q: "rewrite rust in:title is:pull-request stars:>20",
        label: "PR about a Rust rewrite on a starred repo",
    },
];

/// Tunable limits for how aggressively discovery paginates.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub repo_pages: u32,
    pub issue_pages: u32,
    pub max_candidates: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        // Wider net so same-product migrations can surface naturally. Search
        // API is ~30 req/min authenticated; callers should still expect a
        // multi-minute discovery phase at these defaults.
        Self {
            repo_pages: 3,
            issue_pages: 3,
            max_candidates: 500,
        }
    }
}

/// Run all discovery queries and return deduplicated candidates with signals.
pub async fn discover(gh: &GitHub, cfg: &DiscoveryConfig) -> Result<Vec<Candidate>> {
    let mut candidates: HashMap<String, Candidate> = HashMap::new();

    for query in REPO_QUERIES {
        debug!(query = query.q, "repo search");
        let items = match gh.search_repositories(query.q, cfg.repo_pages).await {
            Ok(items) => items,
            Err(e) => {
                warn!(query = query.q, error = %e, "repo search failed; continuing");
                continue;
            }
        };
        for item in items {
            let entry = candidates
                .entry(item.full_name.clone())
                .or_insert_with(|| Candidate {
                    full_name: item.full_name.clone(),
                    ..Default::default()
                });
            // Repo search results carry rich metadata; keep the best we've seen.
            entry.html_url = item.html_url.clone();
            if entry.description.is_none() {
                entry.description = item.description.clone();
            }
            entry.stars = entry.stars.max(item.stargazers_count);
            if entry.created_at.is_none() {
                entry.created_at = item.created_at.clone();
            }
            if entry.pushed_at.is_none() {
                entry.pushed_at = item.pushed_at.clone();
            }
            push_signal(
                entry,
                Signal {
                    kind: "repo-search".to_string(),
                    detail: query.label.to_string(),
                    url: item.html_url,
                },
            );
        }
    }

    for query in ISSUE_QUERIES {
        debug!(query = query.q, "issue search");
        let items = match gh.search_issues(query.q, cfg.issue_pages).await {
            Ok(items) => items,
            Err(e) => {
                warn!(query = query.q, error = %e, "issue search failed; continuing");
                continue;
            }
        };
        for item in items {
            let Some(full_name) = item.repo_full_name() else {
                continue;
            };
            let kind = if item.is_pull_request() {
                "pull-request"
            } else {
                "issue"
            };
            let entry = candidates
                .entry(full_name.clone())
                .or_insert_with(|| Candidate {
                    full_name,
                    ..Default::default()
                });
            push_signal(
                entry,
                Signal {
                    kind: kind.to_string(),
                    detail: format!("{}: {}", query.label, truncate(&item.title, 120)),
                    url: item.html_url,
                },
            );
        }
    }

    let mut out: Vec<Candidate> = candidates.into_values().collect();
    // Prefer candidates with more corroborating signals, then more stars.
    out.sort_by(|a, b| {
        b.signals
            .len()
            .cmp(&a.signals.len())
            .then(b.stars.cmp(&a.stars))
    });
    out.truncate(cfg.max_candidates);

    info!(candidates = out.len(), "discovery complete");
    Ok(out)
}

/// Add a signal, avoiding duplicates that share the same url + detail.
fn push_signal(candidate: &mut Candidate, signal: Signal) {
    let dup = candidate
        .signals
        .iter()
        .any(|s| s.url == signal.url && s.detail == signal.detail);
    if !dup {
        candidate.signals.push(signal);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('\u{2026}');
    out
}

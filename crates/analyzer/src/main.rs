//! ReRust: detect open-source projects being rewritten in Rust.
//!
//! Subcommands:
//!   * `scan`             - discover candidates on GitHub, analyze them, store results
//!   * `reclassify`       - re-derive kind/confidence offline from stored rows
//!   * `backfill-history` - resume-safe commit-history enrichment
//!   * `history`          - debug transition report for one repo
//!   * `build-site`       - render the stored results into a static site
//!
//! Designed to run cheaply on a schedule (e.g. GitHub Actions cron).

mod detect;
mod exemplars;
mod geiger;
mod github;
mod site;
mod store;
mod types;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing::{info, warn};

use detect::commits::LanguageAnalysis;
use detect::enrich::CommitEnrichment;
use detect::heuristics::{self, DiscoveryConfig};
use detect::transitions::{self, HistoryAnalysis};
use detect::{commits, enrich, score};
use github::GitHub;
use store::Store;
use types::{Candidate, Project, Signal};

#[derive(Parser)]
#[command(name = "rerust", version, about = "Detect projects being rewritten in Rust")]
struct Cli {
    /// Path to the SQLite database.
    #[arg(long, default_value = "rerust.db", global = true)]
    db: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan GitHub for Rust rewrite candidates and store scored results.
    Scan(ScanArgs),
    /// Re-derive project kind + confidence from already-stored rows (no network,
    /// no GitHub token). Use after changing the classification heuristic.
    Reclassify,
    /// Render the stored results into a static site.
    BuildSite(BuildSiteArgs),
    /// Analyze one repo's commit history for language transitions (debug/report).
    History(HistoryArgs),
    /// Backfill commit-history metrics for all projects already in the database.
    BackfillHistory(BackfillArgs),
    /// Fetch and score curated exemplar repos from `data/exemplars.txt`, then
    /// optionally run macro-commit history analysis on each.
    ScanExemplars(ScanExemplarsArgs),
}

#[derive(Args)]
struct ScanArgs {
    /// Pages (x100) to fetch per repository search query.
    #[arg(long, default_value_t = 3)]
    repo_pages: u32,
    /// Pages (x100) to fetch per issue/PR search query.
    #[arg(long, default_value_t = 3)]
    issue_pages: u32,
    /// Maximum number of candidates to enrich and score.
    #[arg(long, default_value_t = 500)]
    max_candidates: usize,
    /// Drop results below this confidence (0.0 - 1.0) before storing.
    #[arg(long, default_value_t = score::DEFAULT_MIN_CONFIDENCE)]
    min_confidence: f64,
    /// Measure unsafe-Rust usage per project with cargo-geiger. Off by default:
    /// it shallow-clones and builds each primarily-Rust repo, which is slow and
    /// requires `cargo install cargo-geiger`. See the `geiger` module.
    #[arg(long, default_value_t = false)]
    measure_unsafe: bool,
    /// Enrich projects with commit-history metrics (lines changed, velocity,
    /// experimental AI-assist heuristic). Shallow-clones each repo; off by
    /// default for scan speed. Superseded by [`--analyze-history`] unless you
    /// want the legacy rewrite-window heuristic only.
    #[arg(long, default_value_t = false)]
    enrich_commits: bool,
    /// Walk full commit history and detect massive language transitions.
    /// Uses a blob-less clone + one `git log --numstat` pass. On by default;
    /// pass `--no-analyze-history` to skip (faster scans).
    #[arg(long = "analyze-history", default_value_t = true, action = clap::ArgAction::SetTrue)]
    #[arg(long = "no-analyze-history", action = clap::ArgAction::SetFalse)]
    analyze_history: bool,
}

#[derive(Args)]
struct HistoryArgs {
    /// GitHub repo URL or `owner/name`.
    repo: String,
}

#[derive(Args)]
struct BackfillArgs {
    /// Skip repos with at least this many stars (0 = no limit; analyze all).
    #[arg(long, default_value_t = 0)]
    max_stars: u64,
    /// Per-step clone/log timeout in seconds for normal repos.
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
    /// Per-step timeout for exemplar / macro-commit analysis (large monorepos).
    #[arg(long, default_value_t = 900)]
    macro_timeout_secs: u64,
    /// Stop retrying a repo after this many failed/empty history attempts.
    #[arg(long, default_value_t = 3)]
    max_attempts: u32,
    /// Retry rows previously marked `failed` (otherwise they stay dead-lettered).
    #[arg(long, default_value_t = false)]
    retry_failed: bool,
    /// Process at most this many pending repos per invocation (fault-tolerant batches).
    #[arg(long, default_value_t = 25)]
    batch_size: usize,
    /// Path to exemplar list (`owner/repo` per line).
    #[arg(long, default_value = "data/exemplars.txt")]
    exemplars_file: String,
    /// Re-analyze rows even when `history_status` is already `ok`.
    #[arg(long, default_value_t = false)]
    force: bool,
    /// Measure unsafe-Rust usage with cargo-geiger for primarily-Rust repos.
    /// Runs during backfill for repos missing a measurement; also re-queues
    /// `history_status=ok` rows that were enriched before geiger was enabled.
    #[arg(long, default_value_t = true, action = clap::ArgAction::SetTrue)]
    #[arg(long = "no-measure-unsafe", action = clap::ArgAction::SetFalse)]
    measure_unsafe: bool,
}

#[derive(Args)]
struct ScanExemplarsArgs {
    /// Path to exemplar list (`owner/repo` per line).
    #[arg(long, default_value = "data/exemplars.txt")]
    exemplars_file: String,
    /// Run macro-commit history analysis after scoring each exemplar.
    #[arg(long, default_value_t = true, action = clap::ArgAction::SetTrue)]
    #[arg(long = "no-analyze-history", action = clap::ArgAction::SetFalse)]
    analyze_history: bool,
    /// Per-step timeout for macro history analysis (seconds).
    #[arg(long, default_value_t = 900)]
    macro_timeout_secs: u64,
    /// Re-run macro history even when `history_status` is already `ok`.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Args)]
struct BuildSiteArgs {
    /// Output directory for the generated site.
    #[arg(long, default_value = "docs")]
    out: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rerust=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Scan(args) => scan(&cli.db, args).await,
        Command::Reclassify => reclassify(&cli.db),
        Command::BuildSite(args) => build_site(&cli.db, args),
        Command::History(args) => history(args).await,
        Command::BackfillHistory(args) => backfill_history(&cli.db, args).await,
        Command::ScanExemplars(args) => scan_exemplars(&cli.db, args).await,
    }
}

async fn scan(db_path: &str, args: ScanArgs) -> Result<()> {
    let store = Store::open(db_path)?;
    let gh = GitHub::new()?;
    if !gh.is_authenticated() {
        warn!("running unauthenticated; expect to hit rate limits quickly");
    }

    let cfg = DiscoveryConfig {
        repo_pages: args.repo_pages,
        issue_pages: args.issue_pages,
        max_candidates: args.max_candidates,
    };

    // Unsafe measurement is opt-in and expensive; probe for the tool once so we
    // can warn early and skip the per-repo work when it's unavailable.
    let measure_unsafe = args.measure_unsafe && {
        if geiger::is_available().await {
            info!("cargo-geiger detected; will measure unsafe usage for primarily-Rust repos");
            true
        } else {
            warn!("--measure-unsafe set but `cargo geiger` is unavailable; run `cargo install cargo-geiger`. Skipping unsafe measurement");
            false
        }
    };

    let candidates = heuristics::discover(&gh, &cfg).await?;
    info!(count = candidates.len(), "enriching candidates");

    let mut stored = 0usize;
    for mut candidate in candidates {
        // Always fetch repo details: even candidates that arrived via repo-search
        // (and already carry stars) need forks/open_issues, which only the repo
        // endpoint provides. This also fills metadata for issue/PR-only finds.
        let needs_metadata = candidate.stars == 0 && candidate.description.is_none();
        match gh.get_repo(&candidate.full_name).await {
            Ok(repo) => {
                if needs_metadata {
                    candidate.html_url = repo.html_url;
                    candidate.description = repo.description;
                    candidate.created_at = repo.created_at;
                    candidate.pushed_at = repo.pushed_at;
                }
                candidate.stars = candidate.stars.max(repo.stargazers_count);
                candidate.forks = repo.forks_count;
                // `open_issues_count` bundles issues and PRs; subtract PRs below.
                candidate.open_issues = repo.open_issues_count;
            }
            Err(e) => {
                if needs_metadata {
                    warn!(repo = %candidate.full_name, error = %e, "skipping: repo lookup failed");
                    continue;
                }
                warn!(repo = %candidate.full_name, error = %e, "repo detail lookup failed; metrics may be incomplete");
            }
        }

        // Open PR count comes from a cheap single-result search query.
        match gh.open_pr_count(&candidate.full_name).await {
            Ok(prs) => {
                candidate.open_prs = prs;
                candidate.open_issues = candidate.open_issues.saturating_sub(prs);
            }
            Err(e) => warn!(repo = %candidate.full_name, error = %e, "open PR count lookup failed"),
        }

        // Language composition is the key confirmation signal.
        match gh.get_languages(&candidate.full_name).await {
            Ok(langs) => candidate.languages = langs,
            Err(e) => warn!(repo = %candidate.full_name, error = %e, "language lookup failed"),
        }

        let analysis = commits::analyze(&candidate);

        let repo_url = if candidate.html_url.is_empty() {
            format!("https://github.com/{}", candidate.full_name)
        } else {
            candidate.html_url.clone()
        };

        let history = if args.analyze_history {
            info!(repo = %repo_url, "analyzing commit history for language transitions");
            let opts = transitions::HistoryOptions::standard(geiger::DEFAULT_TIMEOUT);
            transitions::analyze_history(&repo_url, opts).await
        } else {
            HistoryAnalysis::default()
        };

        let enrichment = if args.enrich_commits {
            info!(repo = %repo_url, "enriching commit history (legacy window)");
            enrich::enrich_commits(&repo_url, geiger::DEFAULT_TIMEOUT).await
        } else if history.enrichment.lines_added.is_some() {
            history.enrichment.clone()
        } else {
            CommitEnrichment::default()
        };

        let now = chrono::Utc::now().to_rfc3339();
        let mut project = score::score(&candidate, &analysis, &enrichment, &history, &now);

        if project.confidence < args.min_confidence {
            continue;
        }

        // Preserve the earliest detection timestamp across scans.
        let repo_url = project.repo_url.clone();
        if let Some(first) = store.first_detected(&repo_url)? {
            project.first_detected = first;
        }

        // Only measure repos where Rust is the primary language: geiger needs a
        // buildable cargo crate, and non-Rust-dominant repos rarely are one.
        if measure_unsafe && analysis.rust_is_primary {
            info!(repo = %repo_url, "measuring unsafe usage with cargo-geiger");
            project.unsafe_percentage =
                geiger::measure_unsafe(&repo_url, geiger::DEFAULT_TIMEOUT).await;
        }

        store.upsert(&project)?;
        stored += 1;
    }

    info!(stored, "scan complete");
    Ok(())
}

/// Re-run classification + scoring over the rows already in the database,
/// without hitting the network. This lets a changed heuristic (e.g. the
/// rewrite-vs-replacement taxonomy) take effect on real scan data without a
/// fresh `scan` (which would require a `GITHUB_TOKEN`).
///
/// Everything the scorer needs is already persisted: the description and raw
/// signals feed the textual classifier, and `rust_percentage` /
/// `original_language` reconstruct the language analysis. The one field not
/// stored verbatim is whether Rust was the single largest language; we
/// approximate `rust_is_primary` as "no displaced language, or Rust holds a
/// majority", which is faithful for the classifier and scorer.
fn reclassify(db_path: &str) -> Result<()> {
    let store = Store::open(db_path)?;
    let projects = store.all()?;
    let now = chrono::Utc::now().to_rfc3339();

    let total = projects.len();
    let mut rewrites = 0usize;
    let mut replacements = 0usize;
    let mut dropped = 0usize;
    for project in &projects {
        let candidate = candidate_from_project(project);
        let analysis = analysis_from_project(project);
        let enrichment = enrichment_from_project(project);
        let history = history_from_project(project);

        let mut rescored = score::score(&candidate, &analysis, &enrichment, &history, &now);
        // Preserve history timestamps; only the derived fields should change.
        rescored.first_detected = project.first_detected.clone();
        rescored.last_seen = project.last_seen.clone();

        match rescored.project_kind.as_str() {
            "rewrite" => {
                rewrites += 1;
                store.upsert(&rescored)?;
            }
            "replacement" => {
                replacements += 1;
                store.upsert(&rescored)?;
            }
            // Neither: no genuine cross-language provenance. Drop it from the
            // store so the site stays free of native-Rust noise and Rust-crate
            // compatibility shims.
            _ => {
                dropped += 1;
                store.delete(&project.repo_url)?;
            }
        }
    }

    info!(
        total,
        rewrites,
        replacements,
        dropped,
        remaining = rewrites + replacements,
        "reclassified stored projects"
    );
    Ok(())
}

/// Rebuild a [`Candidate`] from a stored [`Project`] for re-scoring. Only the
/// fields the scorer/classifier read are populated; `languages` is left empty
/// because scoring consumes the pre-computed [`LanguageAnalysis`] instead.
fn candidate_from_project(p: &Project) -> Candidate {
    let full_name = p
        .repo_url
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_end_matches('/')
        .to_string();
    Candidate {
        full_name,
        html_url: p.repo_url.clone(),
        description: p.description.clone(),
        stars: p.stars,
        forks: p.forks,
        open_issues: p.open_issues,
        open_prs: p.open_prs,
        languages: Vec::new(),
        created_at: None,
        pushed_at: None,
        signals: p.signals.clone(),
        unsafe_percentage: p.unsafe_percentage,
        named_origin: p.named_origin.clone(),
    }
}

/// Reconstruct the language analysis from stored composition fields.
fn analysis_from_project(p: &Project) -> LanguageAnalysis {
    LanguageAnalysis {
        rust_percentage: p.rust_percentage,
        original_language: p.original_language.clone(),
        rust_is_primary: p.original_language.is_none() || p.rust_percentage >= 50.0,
    }
}

/// Reconstruct commit-enrichment metrics from stored fields.
fn enrichment_from_project(p: &Project) -> CommitEnrichment {
    CommitEnrichment {
        lines_added: p.lines_added,
        lines_removed: p.lines_removed,
        rewrite_velocity: p.rewrite_velocity,
        ai_assist_score: p.ai_assist_score,
        rewrite_duration_days: p.rewrite_duration_days,
        commit_count: p.commit_count,
    }
}

fn history_from_project(p: &Project) -> HistoryAnalysis {
    HistoryAnalysis {
        enrichment: enrichment_from_project(p),
        from_language: p.history_from_language.clone(),
        rust_pct_before: p.history_rust_before,
        rust_pct_after: p.history_rust_after,
        transition_magnitude: p.transition_magnitude,
        total_commits: p.total_commits_analyzed.unwrap_or(0),
        strong_transition: p.transition_magnitude.unwrap_or(0.0) >= 25.0
            && p.history_rust_after.unwrap_or(0.0) >= 40.0
            && p.history_from_language.is_some()
            && p.history_rust_before.unwrap_or(100.0) <= 35.0,
    }
}

async fn backfill_history(db_path: &str, args: BackfillArgs) -> Result<()> {
    let _lock = acquire_backfill_lock(db_path)?;
    let store = Store::open(db_path)?;
    let exemplar_set = load_exemplar_set(&args.exemplars_file)?;
    let measure_unsafe = args.measure_unsafe && {
        if geiger::is_available().await {
            info!("cargo-geiger detected; will measure unsafe usage for primarily-Rust repos");
            true
        } else {
            warn!("--measure-unsafe set but `cargo geiger` is unavailable; run `cargo install cargo-geiger`. Skipping unsafe measurement");
            false
        }
    };
    let mut projects = store.all()?;
    projects.sort_by(|a, b| {
        exemplars::backfill_priority(&a.repo_url, a.stars, a.confidence, &exemplar_set).cmp(
            &exemplars::backfill_priority(&b.repo_url, b.stars, b.confidence, &exemplar_set),
        )
    });

    let now = chrono::Utc::now().to_rfc3339();
    let total = projects.len();
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut skipped_huge = 0usize;
    let mut failed = 0usize;
    let mut dead_lettered = 0usize;
    let mut processed = 0usize;

    let pending: Vec<_> = projects
        .iter()
        .filter(|p| backfill_pending(p, &args, measure_unsafe, &exemplar_set))
        .collect();
    let pending_count = pending.len();

    for project in projects {
        if args.max_stars > 0 && project.stars >= args.max_stars {
            skipped_huge += 1;
            continue;
        }

        if !backfill_pending(&project, &args, measure_unsafe, &exemplar_set) {
            if history_already_ok(&project) {
                skipped += 1;
            } else {
                dead_lettered += 1;
            }
            continue;
        }

        if processed >= args.batch_size {
            break;
        }
        processed += 1;

        let analysis = analysis_from_project(&project);
        let history_ok = history_already_ok(&project) && !args.force;
        let needs_geiger =
            measure_unsafe && analysis.rust_is_primary && project.unsafe_percentage.is_none();

        // Geiger-only pass for repos already enriched but never measured.
        // Skip exemplar monorepos here — they are usually workspace roots where
        // `cargo geiger` cannot run, and they would otherwise starve the queue.
        if history_ok {
            if needs_geiger && !exemplars::is_exemplar(&project.repo_url, &exemplar_set) {
                info!(
                    repo = %project.repo_url,
                    progress = format_args!("{processed}/{pending_count}"),
                    "measuring unsafe usage with cargo-geiger"
                );
                let pct =
                    geiger::measure_unsafe(&project.repo_url, geiger::DEFAULT_TIMEOUT).await;
                if let Some(pct) = pct {
                    let mut updated_project = project.clone();
                    updated_project.unsafe_percentage = Some(pct);
                    if let Err(e) = store.upsert(&updated_project) {
                        warn!(repo = %project.repo_url, error = %e, "geiger upsert failed");
                    } else {
                        updated += 1;
                        info!(repo = %project.repo_url, unsafe_percentage = pct, "unsafe measured");
                    }
                }
            }
            continue;
        }

        let prior_attempts = project.history_attempts.unwrap_or(0);
        let macro_repo = exemplars::is_exemplar(&project.repo_url, &exemplar_set);
        let step_secs = if macro_repo {
            args.macro_timeout_secs
        } else {
            args.timeout_secs
        };
        let opts = if macro_repo {
            transitions::HistoryOptions::r#macro(std::time::Duration::from_secs(step_secs))
        } else {
            transitions::HistoryOptions::standard(std::time::Duration::from_secs(step_secs))
        };

        info!(
            repo = %project.repo_url,
            stars = project.stars,
            macro_mode = macro_repo,
            progress = format_args!("{processed}/{pending_count}"),
            "backfilling commit history"
        );

        let history = transitions::analyze_history(&project.repo_url, opts).await;
        if history.total_commits == 0 {
            let attempts = prior_attempts.saturating_add(1);
            let err = "no commits analyzed (clone/log failed or empty)";
            warn!(repo = %project.repo_url, attempts, "{err}");
            if let Err(e) =
                store.mark_history_attempt(&project.repo_url, "failed", Some(err), &now, attempts)
            {
                warn!(repo = %project.repo_url, error = %e, "could not mark history failure");
            }
            failed += 1;
            continue;
        }

        let candidate = candidate_from_project(&project);
        let analysis = analysis_from_project(&project);
        let enrichment = if history.enrichment.lines_added.is_some() {
            history.enrichment.clone()
        } else {
            enrichment_from_project(&project)
        };

        let mut rescored = score::score(&candidate, &analysis, &enrichment, &history, &now);
        rescored.first_detected = project.first_detected.clone();
        rescored.last_seen = project.last_seen.clone();
        rescored.history_status = Some("ok".into());
        rescored.history_error = None;
        rescored.history_attempted_at = Some(now.clone());
        rescored.history_attempts = Some(0);

        if needs_geiger {
            info!(repo = %project.repo_url, "measuring unsafe usage with cargo-geiger");
            rescored.unsafe_percentage =
                geiger::measure_unsafe(&project.repo_url, geiger::DEFAULT_TIMEOUT).await;
        }

        if let Err(e) = store.upsert(&rescored) {
            warn!(repo = %project.repo_url, error = %e, "upsert failed; continuing");
            let attempts = prior_attempts.saturating_add(1);
            let _ = store.mark_history_attempt(
                &project.repo_url,
                "failed",
                Some(&format!("upsert: {e}")),
                &now,
                attempts,
            );
            failed += 1;
            continue;
        }
        let _ = store.mark_history_attempt(&project.repo_url, "ok", None, &now, 0);
        updated += 1;
        info!(
            repo = %project.repo_url,
            commits = history.total_commits,
            from = ?history.from_language,
            magnitude = ?history.transition_magnitude,
            kind = %rescored.project_kind,
            "history backfilled"
        );
    }

    info!(
        total,
        pending = pending_count,
        batch = processed,
        updated,
        skipped,
        skipped_huge,
        failed,
        dead_lettered,
        max_stars = args.max_stars,
        "backfill-history batch complete"
    );
    Ok(())
}

fn load_exemplar_set(path: &str) -> Result<std::collections::HashSet<String>> {
    let slugs = exemplars::load(std::path::Path::new(path))?;
    Ok(slugs.into_iter().collect())
}

fn backfill_pending(
    p: &Project,
    args: &BackfillArgs,
    measure_unsafe: bool,
    exemplar_set: &std::collections::HashSet<String>,
) -> bool {
    if args.force {
        return true;
    }
    let needs_geiger = measure_unsafe
        && p.unsafe_percentage.is_none()
        && (p.original_language.is_none() || p.rust_percentage >= 50.0)
        && !exemplars::is_exemplar(&p.repo_url, exemplar_set);
    if history_already_ok(p) {
        return needs_geiger;
    }
    if !args.retry_failed
        && p.history_status.as_deref() == Some("failed")
        && p.history_attempts.unwrap_or(0) >= args.max_attempts
    {
        return false;
    }
    if p.history_status.as_deref() == Some("skipped_huge") && args.max_stars == 0 {
        // Re-queue repos previously star-skipped when running without a cap.
        return true;
    }
    true
}

/// Fetch curated exemplars from GitHub, score them, and run macro history analysis.
async fn scan_exemplars(db_path: &str, args: ScanExemplarsArgs) -> Result<()> {
    let store = Store::open(db_path)?;
    let gh = GitHub::new()?;
    if !gh.is_authenticated() {
        warn!("GITHUB_TOKEN unset; exemplar scan may hit rate limits");
    }

    let slugs = exemplars::load(std::path::Path::new(&args.exemplars_file))?;
    let exemplar_set: std::collections::HashSet<_> = slugs.iter().cloned().collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut stored = 0usize;
    let mut enriched = 0usize;
    let mut skipped_history = 0usize;

    for slug in &slugs {
        let full_name = slug.clone();
        let repo_url = format!("https://github.com/{full_name}");
        let existing = store.get(&repo_url)?;
        if existing.as_ref().map(history_already_ok) == Some(true) && !args.force {
            info!(repo = %full_name, "skipping exemplar macro history (already ok)");
            skipped_history += 1;
            continue;
        }

        info!(repo = %full_name, "scanning exemplar");

        let mut candidate = Candidate {
            full_name: full_name.clone(),
            html_url: format!("https://github.com/{full_name}"),
            signals: vec![Signal {
                kind: "exemplar".into(),
                detail: "curated exemplar rewrite (priority target)".into(),
                url: format!("https://github.com/{full_name}"),
            }],
            ..Default::default()
        };

        match gh.get_repo(&full_name).await {
            Ok(repo) => {
                candidate.description = repo.description;
                candidate.stars = repo.stargazers_count;
                candidate.forks = repo.forks_count;
                candidate.open_issues = repo.open_issues_count;
                candidate.created_at = repo.created_at;
                candidate.pushed_at = repo.pushed_at;
            }
            Err(e) => {
                warn!(repo = %full_name, error = %e, "exemplar repo lookup failed; skipping");
                continue;
            }
        }

        if let Ok(prs) = gh.open_pr_count(&full_name).await {
            candidate.open_prs = prs;
            if candidate.open_issues > prs {
                candidate.open_issues -= prs;
            }
        }

        if let Ok(langs) = gh.get_languages(&full_name).await {
            candidate.languages = langs;
        }

        let analysis = commits::analyze(&candidate);
        let repo_url = candidate.html_url.clone();

        let history = if args.analyze_history {
            let opts = transitions::HistoryOptions::r#macro(std::time::Duration::from_secs(
                args.macro_timeout_secs,
            ));
            info!(repo = %repo_url, "macro-commit history analysis");
            transitions::analyze_history(&repo_url, opts).await
        } else {
            HistoryAnalysis::default()
        };

        let enrichment = if history.enrichment.lines_added.is_some() {
            history.enrichment.clone()
        } else {
            CommitEnrichment::default()
        };

        let mut project = score::score(&candidate, &analysis, &enrichment, &history, &now);
        if let Some(first) = store.first_detected(&project.repo_url)? {
            project.first_detected = first;
        }
        if args.analyze_history && history.total_commits > 0 {
            project.history_status = Some("ok".into());
            project.history_error = None;
            project.history_attempted_at = Some(now.clone());
            project.history_attempts = Some(0);
            enriched += 1;
        }

        store.upsert(&project)?;
        stored += 1;
        info!(
            repo = %project.repo_url,
            kind = %project.project_kind,
            confidence = project.confidence,
            commits = history.total_commits,
            exemplar = exemplars::is_exemplar(&project.repo_url, &exemplar_set),
            "exemplar stored"
        );
    }

    info!(
        stored,
        enriched,
        skipped_history,
        total = slugs.len(),
        "scan-exemplars complete"
    );
    Ok(())
}

/// True when transition analysis already completed successfully for this row.
fn history_already_ok(p: &Project) -> bool {
    p.history_status.as_deref() == Some("ok")
}

/// Exclusive lock so two backfills cannot corrupt the same SQLite file.
fn acquire_backfill_lock(db_path: &str) -> Result<std::fs::File> {
    use std::io::Write;

    let lock_path = format!("{db_path}.backfill.lock");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .with_context(|| format!("open lock file {lock_path}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            anyhow::bail!(
                "another backfill-history is already running (lock: {lock_path})"
            );
        }
    }

    writeln!(file, "{}", std::process::id())?;
    Ok(file)
}

async fn history(args: HistoryArgs) -> Result<()> {
    let repo_url = normalize_repo_url(&args.repo);
    info!(repo = %repo_url, "cloning and walking commit history");
    let h = transitions::analyze_history(
        &repo_url,
        transitions::HistoryOptions::r#macro(geiger::DEFAULT_TIMEOUT),
    )
    .await;

    println!("Repository: {repo_url}");
    println!("Commits analyzed: {}", h.total_commits);
    if let Some(from) = &h.from_language {
        println!("From language (early history): {from}");
    }
    if let (Some(before), Some(after)) = (h.rust_pct_before, h.rust_pct_after) {
        println!("Rust share: {before:.1}% → {after:.1}%");
    }
    if let Some(mag) = h.transition_magnitude {
        println!("Transition magnitude: {mag:.1} pts");
    }
    println!("Strong transition: {}", h.strong_transition);

    let e = &h.enrichment;
    if e.lines_added.is_some() || e.lines_removed.is_some() {
        println!();
        println!("Transition window:");
        println!("  lines added:   {}", e.lines_added.unwrap_or(0));
        println!("  lines removed: {}", e.lines_removed.unwrap_or(0));
        println!("  commits:       {}", e.commit_count.unwrap_or(0));
        if let Some(v) = e.rewrite_velocity {
            println!("  velocity:      {v:.0} lines/day");
        }
        if let Some(d) = e.rewrite_duration_days {
            println!("  duration:      {d} days");
        }
        if let Some(ai) = e.ai_assist_score {
            println!("  AI-assist:     {ai:.2} (experimental)");
        }
    }
    Ok(())
}

fn normalize_repo_url(repo: &str) -> String {
    if repo.starts_with("https://") || repo.starts_with("http://") {
        repo.trim_end_matches('/').to_string()
    } else {
        format!("https://github.com/{}", repo.trim_matches('/'))
    }
}

fn build_site(db_path: &str, args: BuildSiteArgs) -> Result<()> {
    let store = Store::open(db_path)?;
    let exemplar_set = load_exemplar_set("data/exemplars.txt").unwrap_or_default();
    let mut projects: Vec<Project> = store
        .all()?
        .into_iter()
        .filter(|p| p.project_kind != "neither" && p.confidence > 0.0)
        .collect();

    // Exemplars first, then rewrites, then confidence / stars.
    projects.sort_by(|a, b| {
        let ae = exemplars::is_exemplar(&a.repo_url, &exemplar_set) as u8;
        let be = exemplars::is_exemplar(&b.repo_url, &exemplar_set) as u8;
        be.cmp(&ae)
            .then_with(|| {
                let ar = (a.project_kind == "rewrite") as u8;
                let br = (b.project_kind == "rewrite") as u8;
                br.cmp(&ar)
            })
            .then(
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(b.stars.cmp(&a.stars))
    });

    for p in &mut projects {
        p.exemplar = exemplars::is_exemplar(&p.repo_url, &exemplar_set);
    }

    site::build(&args.out, &projects)?;
    info!(count = projects.len(), out = %args.out, "site built");
    Ok(())
}

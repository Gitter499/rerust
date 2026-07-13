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
mod origins;
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
use detect::{commits, score};
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
    /// Merge enrichment rows from shard databases into a base database
    /// (preferring `history_status=ok`, then newest attempt).
    MergeDb(MergeDbArgs),
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
    /// Process only repos in this shard (`INDEX/TOTAL`, e.g. `3/8`). Enables
    /// parallel backfill across Actions runners without lock contention.
    #[arg(long, value_name = "INDEX/TOTAL")]
    shard: Option<String>,
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

#[derive(Args)]
struct MergeDbArgs {
    /// Destination database (updated in place).
    #[arg(long)]
    into: String,
    /// Shard database paths produced by parallel `backfill-history --shard`.
    #[arg(required = true)]
    shards: Vec<String>,
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
        Command::MergeDb(args) => merge_db(args),
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

    let measure_unsafe = resolve_measure_unsafe(args.measure_unsafe).await;
    let candidates = heuristics::discover(&gh, &cfg).await?;
    info!(count = candidates.len(), "enriching candidates");

    let mut stored = 0usize;
    for mut candidate in candidates {
        if !enrich_candidate_from_github(&gh, &mut candidate).await {
            continue;
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

        let enrichment = history_enrichment(&history);

        let now = chrono::Utc::now().to_rfc3339();
        let mut project = score::score(&candidate, &analysis, &enrichment, &history, &now);

        if project.confidence < args.min_confidence {
            continue;
        }

        let repo_url = project.repo_url.clone();
        if let Some(first) = store.first_detected(&repo_url)? {
            project.first_detected = first;
        }

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

/// Fetch repo metadata, open-PR count, and language bytes. Returns false when
/// a metadata-less candidate cannot be looked up and should be skipped.
async fn enrich_candidate_from_github(gh: &GitHub, candidate: &mut Candidate) -> bool {
    let needs_metadata = candidate.stars == 0 && candidate.description.is_none();
    match gh.get_repo(&candidate.full_name).await {
        Ok(repo) => {
            if needs_metadata {
                candidate.html_url = repo.html_url;
                candidate.description = repo.description;
            }
            candidate.stars = candidate.stars.max(repo.stargazers_count);
            candidate.forks = repo.forks_count;
            candidate.open_issues = repo.open_issues_count;
        }
        Err(e) => {
            if needs_metadata {
                warn!(repo = %candidate.full_name, error = %e, "skipping: repo lookup failed");
                return false;
            }
            warn!(repo = %candidate.full_name, error = %e, "repo detail lookup failed; metrics may be incomplete");
        }
    }

    match gh.open_pr_count(&candidate.full_name).await {
        Ok(prs) => {
            candidate.open_prs = prs;
            candidate.open_issues = candidate.open_issues.saturating_sub(prs);
        }
        Err(e) => warn!(repo = %candidate.full_name, error = %e, "open PR count lookup failed"),
    }

    match gh.get_languages(&candidate.full_name).await {
        Ok(langs) => candidate.languages = langs,
        Err(e) => warn!(repo = %candidate.full_name, error = %e, "language lookup failed"),
    }
    true
}

async fn resolve_measure_unsafe(requested: bool) -> bool {
    if !requested {
        return false;
    }
    if geiger::is_available().await {
        info!("cargo-geiger detected; will measure unsafe usage for primarily-Rust repos");
        true
    } else {
        warn!("--measure-unsafe set but `cargo geiger` is unavailable; run `cargo install cargo-geiger`. Skipping unsafe measurement");
        false
    }
}

/// Re-derive kind/confidence from stored rows (no network). Reconstructs the
/// scorer inputs from persisted fields; `rust_is_primary` is approximated as
/// "no displaced language, or Rust ≥ 50%".
fn reclassify(db_path: &str) -> Result<()> {
    let store = Store::open(db_path)?;
    let projects = store.all()?;
    let now = chrono::Utc::now().to_rfc3339();

    let total = projects.len();
    let mut rewrites = 0usize;
    let mut replacements = 0usize;
    let mut dropped = 0usize;
    for project in &projects {
        let enrichment = enrichment_from_project(project);
        let history = history_from_project(project);
        let mut rescored = rescore(project, &enrichment, &history, &now);
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

fn candidate_from_project(p: &Project) -> Candidate {
    let full_name = p
        .repo_url
        .trim()
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
        signals: p.signals.clone(),
    }
}

fn analysis_from_project(p: &Project) -> LanguageAnalysis {
    LanguageAnalysis {
        rust_percentage: p.rust_percentage,
        original_language: p.original_language.clone(),
        rust_is_primary: p.original_language.is_none() || p.rust_percentage >= 50.0,
    }
}

fn enrichment_from_project(p: &Project) -> CommitEnrichment {
    CommitEnrichment {
        lines_added: p.lines_added,
        lines_removed: p.lines_removed,
        rewrite_velocity: p.rewrite_velocity,
        ai_assist_score: p.ai_assist_score,
        ai_agents: p.ai_agents.clone(),
        rewrite_duration_days: p.rewrite_duration_days,
        commit_count: p.commit_count,
    }
}

fn history_enrichment(history: &HistoryAnalysis) -> CommitEnrichment {
    let e = &history.enrichment;
    if e.lines_added.is_some() || !e.ai_agents.is_empty() {
        e.clone()
    } else {
        CommitEnrichment::default()
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
        strong_transition: transitions::strong_transition_from_stored(
            p.transition_magnitude,
            p.history_rust_before,
            p.history_rust_after,
            p.history_from_language.as_deref(),
        ),
    }
}

fn rescore(
    p: &Project,
    enrichment: &CommitEnrichment,
    history: &HistoryAnalysis,
    now: &str,
) -> Project {
    score::score(
        &candidate_from_project(p),
        &analysis_from_project(p),
        enrichment,
        history,
        now,
    )
}

async fn backfill_history(db_path: &str, args: BackfillArgs) -> Result<()> {
    let shard = parse_shard(args.shard.as_deref())?;
    // Shard workers own a private DB copy — skip the exclusive lock.
    let _lock = if shard.is_some() {
        None
    } else {
        Some(acquire_backfill_lock(db_path)?)
    };
    let store = Store::open(db_path)?;
    let exemplar_set: std::collections::HashSet<_> =
        exemplars::load(std::path::Path::new(&args.exemplars_file))?
            .into_iter()
            .collect();
    let measure_unsafe = resolve_measure_unsafe(args.measure_unsafe).await;
    let mut projects = store.all()?;
    if let Some((index, total)) = shard {
        projects.retain(|p| shard_owns(&p.repo_url, index, total));
        info!(shard = index, shards = total, repos = projects.len(), "backfill shard filter applied");
    }
    projects.sort_by(|a, b| {
        exemplars::backfill_priority(&a.repo_url, a.stars, a.confidence, &exemplar_set).cmp(
            &exemplars::backfill_priority(&b.repo_url, b.stars, b.confidence, &exemplar_set),
        )
    });

    let now = chrono::Utc::now().to_rfc3339();
    let total = projects.len();
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut skipped_star_cap = 0usize;
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
            skipped_star_cap += 1;
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

        // Already enriched: optional geiger-only pass (skip exemplar monorepos).
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

        let enrichment = if history.enrichment.lines_added.is_some() {
            history.enrichment.clone()
        } else {
            enrichment_from_project(&project)
        };

        let mut rescored = rescore(&project, &enrichment, &history, &now);
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
        skipped_star_cap,
        failed,
        dead_lettered,
        max_stars = args.max_stars,
        "backfill-history batch complete"
    );
    Ok(())
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
    true
}

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

        if !enrich_candidate_from_github(&gh, &mut candidate).await {
            continue;
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

        let enrichment = history_enrichment(&history);
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

fn history_already_ok(p: &Project) -> bool {
    p.history_status.as_deref() == Some("ok")
}

fn parse_shard(raw: Option<&str>) -> Result<Option<(u32, u32)>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (index, total) = raw
        .split_once('/')
        .with_context(|| format!("invalid --shard {raw:?}, expected INDEX/TOTAL"))?;
    let index: u32 = index.parse().context("shard index")?;
    let total: u32 = total.parse().context("shard total")?;
    anyhow::ensure!(total > 0, "shard total must be > 0");
    anyhow::ensure!(index < total, "shard index must be < total");
    Ok(Some((index, total)))
}

fn shard_owns(repo_url: &str, index: u32, total: u32) -> bool {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in repo_url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % u64::from(total)) as u32 == index
}

fn prefer_enriched(a: &Project, b: &Project) -> Project {
    let rank = |p: &Project| -> (u8, String) {
        let status = match p.history_status.as_deref() {
            Some("ok") => 2,
            Some("failed") => 1,
            _ => 0,
        };
        (status, p.history_attempted_at.clone().unwrap_or_default())
    };
    if rank(b) > rank(a) {
        b.clone()
    } else {
        a.clone()
    }
}

fn merge_db(args: MergeDbArgs) -> Result<()> {
    let base = Store::open(&args.into)?;
    let mut by_url: std::collections::HashMap<String, Project> = base
        .all()?
        .into_iter()
        .map(|p| (p.repo_url.clone(), p))
        .collect();

    for path in &args.shards {
        let shard = Store::open(path)?;
        let mut merged = 0usize;
        for project in shard.all()? {
            let entry = by_url
                .entry(project.repo_url.clone())
                .or_insert_with(|| project.clone());
            let preferred = prefer_enriched(entry, &project);
            if preferred.history_status != entry.history_status
                || preferred.history_attempted_at != entry.history_attempted_at
                || preferred.transition_magnitude != entry.transition_magnitude
            {
                merged += 1;
            }
            *entry = preferred;
        }
        info!(shard = %path, merged, "merged shard into base map");
    }

    let mut written = 0usize;
    for project in by_url.values() {
        base.upsert(project)?;
        written += 1;
    }
    info!(into = %args.into, written, shards = args.shards.len(), "merge-db complete");
    Ok(())
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
        if !e.ai_agents.is_empty() {
            println!("  AI agents:     {}", e.ai_agents.join(", "));
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
    let exemplar_set: std::collections::HashSet<_> =
        exemplars::load(std::path::Path::new("data/exemplars.txt"))
            .unwrap_or_default()
            .into_iter()
            .collect();
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
        if let Some(origin) = origins::lookup(&p.repo_url) {
            p.named_origin = Some(origin);
        }
    }

    site::build(&args.out, &projects)?;
    info!(count = projects.len(), out = %args.out, "site built");
    Ok(())
}

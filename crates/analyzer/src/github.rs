//! Minimal GitHub REST + Search API client.
//!
//! Handles authentication via the `GITHUB_TOKEN` environment variable and
//! conservatively respects both primary and secondary rate limits. The Search
//! API is limited to 30 requests/minute when authenticated (10/min otherwise),
//! so callers should paginate frugally.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use tracing::{debug, warn};

const API_ROOT: &str = "https://api.github.com";
const UA: &str = "rerust-analyzer (+https://github.com/rafo/rerust)";

/// A single repository result from the Search API or repo endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoItem {
    pub full_name: String,
    pub html_url: String,
    pub description: Option<String>,
    #[serde(default)]
    pub stargazers_count: u64,
    #[serde(default)]
    pub forks_count: u64,
    /// Combined count of open issues *and* open pull requests.
    #[serde(default)]
    pub open_issues_count: u64,
}

#[derive(Debug, Deserialize)]
struct RepoSearchResponse {
    #[serde(default)]
    items: Vec<RepoItem>,
}

/// A single issue/PR result from the issue Search API.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueItem {
    pub title: String,
    pub html_url: String,
    /// API URL of the owning repository, e.g. `.../repos/owner/name`.
    pub repository_url: String,
    /// Present only when the issue is actually a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

impl IssueItem {
    /// Extract "owner/name" from the API `repository_url`.
    pub fn repo_full_name(&self) -> Option<String> {
        self.repository_url
            .rsplit("/repos/")
            .next()
            .filter(|s| s.contains('/'))
            .map(|s| s.to_string())
    }

    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Deserialize)]
struct IssueSearchResponse {
    #[serde(default)]
    items: Vec<IssueItem>,
}

/// Count-only view of a Search API response (`total_count` is exact for the query).
#[derive(Debug, Deserialize)]
struct SearchCountResponse {
    #[serde(default)]
    total_count: u64,
}

pub struct GitHub {
    client: reqwest::Client,
    authenticated: bool,
}

impl GitHub {
    /// Build a client, reading `GITHUB_TOKEN` from the environment if present.
    pub fn new() -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(UA));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );

        let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
        let authenticated = token.is_some();
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid GITHUB_TOKEN value")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        } else {
            warn!("GITHUB_TOKEN not set; using unauthenticated requests with very low rate limits");
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            authenticated,
        })
    }

    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Perform a GET with retry/backoff on rate limits and transient errors.
    async fn get(&self, url: &str) -> Result<Response> {
        // Up to 5 attempts, honoring Retry-After and X-RateLimit-Reset.
        for attempt in 0..5u32 {
            let resp = self
                .client
                .get(url)
                .send()
                .await
                .with_context(|| format!("request failed: {url}"))?;

            let status = resp.status();

            if status.is_success() {
                self.maybe_throttle(&resp).await;
                return Ok(resp);
            }

            // Rate limited (primary or secondary) -> wait and retry.
            if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
                let wait = self.rate_limit_wait(&resp);
                warn!(url, attempt, secs = wait.as_secs(), "rate limited; backing off");
                tokio::time::sleep(wait).await;
                continue;
            }

            // Server-side hiccup -> exponential backoff.
            if status.is_server_error() {
                let backoff = Duration::from_secs(2u64.pow(attempt));
                warn!(url, %status, attempt, "server error; retrying");
                tokio::time::sleep(backoff).await;
                continue;
            }

            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {url} failed with {status}: {body}");
        }

        anyhow::bail!("GET {url} exhausted retries due to rate limiting")
    }

    /// If we're close to exhausting the current quota, pause until reset.
    async fn maybe_throttle(&self, resp: &Response) {
        let remaining = header_u64(resp, "x-ratelimit-remaining");
        if let Some(0) = remaining {
            let wait = self.rate_limit_wait(resp);
            debug!(secs = wait.as_secs(), "quota exhausted; waiting for reset");
            tokio::time::sleep(wait).await;
        }
    }

    /// Compute how long to wait based on `Retry-After` or `X-RateLimit-Reset`.
    fn rate_limit_wait(&self, resp: &Response) -> Duration {
        if let Some(retry_after) = header_u64(resp, "retry-after") {
            return Duration::from_secs(retry_after.clamp(1, 300));
        }
        if let Some(reset) = header_u64(resp, "x-ratelimit-reset") {
            let now = chrono::Utc::now().timestamp() as u64;
            if reset > now {
                return Duration::from_secs((reset - now + 1).clamp(1, 300));
            }
        }
        // Fallback: search API refills every 60s.
        Duration::from_secs(60)
    }

    /// Search repositories. Returns up to `max_pages * 100` results.
    pub async fn search_repositories(
        &self,
        query: &str,
        max_pages: u32,
    ) -> Result<Vec<RepoItem>> {
        let mut out = Vec::new();
        for page in 1..=max_pages {
            let url = format!(
                "{API_ROOT}/search/repositories?q={}&sort=stars&order=desc&per_page=100&page={page}",
                urlencoding::encode(query)
            );
            let resp = self.get(&url).await?;
            let parsed: RepoSearchResponse = resp.json().await.context("parse repo search")?;
            let count = parsed.items.len();
            out.extend(parsed.items);
            if count < 100 {
                break;
            }
        }
        debug!(query, results = out.len(), "repo search complete");
        Ok(out)
    }

    /// Search issues and pull requests. Returns up to `max_pages * 100` results.
    pub async fn search_issues(&self, query: &str, max_pages: u32) -> Result<Vec<IssueItem>> {
        let mut out = Vec::new();
        for page in 1..=max_pages {
            let url = format!(
                "{API_ROOT}/search/issues?q={}&per_page=100&page={page}",
                urlencoding::encode(query)
            );
            let resp = self.get(&url).await?;
            let parsed: IssueSearchResponse = resp.json().await.context("parse issue search")?;
            let count = parsed.items.len();
            out.extend(parsed.items);
            if count < 100 {
                break;
            }
        }
        debug!(query, results = out.len(), "issue search complete");
        Ok(out)
    }

    /// Fetch full repository metadata for "owner/name".
    pub async fn get_repo(&self, full_name: &str) -> Result<RepoItem> {
        let url = format!("{API_ROOT}/repos/{full_name}");
        let resp = self.get(&url).await?;
        resp.json().await.context("parse repo details")
    }

    /// Count open pull requests for "owner/name" via the search API.
    ///
    /// The repo endpoint's `open_issues_count` lumps issues and PRs together, so
    /// this cheap single-result search recovers the pure open-PR total. Uses the
    /// same rate-limit-aware `get()` path (search cap: 30 req/min authenticated).
    pub async fn open_pr_count(&self, full_name: &str) -> Result<u64> {
        let query = format!("repo:{full_name} type:pr state:open");
        let url = format!(
            "{API_ROOT}/search/issues?q={}&per_page=1",
            urlencoding::encode(&query)
        );
        let resp = self.get(&url).await?;
        let parsed: SearchCountResponse = resp.json().await.context("parse open-pr count")?;
        Ok(parsed.total_count)
    }

    /// Fetch the language byte-breakdown for "owner/name".
    pub async fn get_languages(&self, full_name: &str) -> Result<Vec<(String, u64)>> {
        let url = format!("{API_ROOT}/repos/{full_name}/languages");
        let resp = self.get(&url).await?;
        let map: std::collections::BTreeMap<String, u64> =
            resp.json().await.context("parse languages")?;
        let mut langs: Vec<(String, u64)> = map.into_iter().collect();
        langs.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        Ok(langs)
    }
}

fn header_u64(resp: &Response, name: &str) -> Option<u64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

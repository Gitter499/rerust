//! Curated exemplar rewrite repositories — priority targets for discovery and
//! macro-commit enrichment. Loaded from `data/exemplars.txt` at runtime.

use std::collections::HashSet;
use std::path::Path;

/// A normalized `owner/repo` slug (lowercase owner and name).
pub fn normalize_slug(slug: &str) -> String {
    slug.trim()
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Load exemplar slugs from a text file (`owner/repo` per line, `#` comments).
pub fn load(path: &Path) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read exemplars file {}", path.display()))?;
    Ok(parse_lines(&raw))
}

fn parse_lines(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let slug = normalize_slug(line);
        if seen.insert(slug.clone()) {
            out.push(slug);
        }
    }
    out
}

/// True when `repo_url` or `owner/name` matches a known exemplar.
pub fn is_exemplar(repo_url: &str, set: &HashSet<String>) -> bool {
    set.contains(&normalize_slug(repo_url))
}

/// Priority key for backfill ordering (lower = sooner). Exemplars first, then
/// high-confidence high-star repos.
pub fn backfill_priority(
    repo_url: &str,
    stars: u64,
    confidence: f64,
    exemplars: &HashSet<String>,
) -> (u8, u64, u64) {
    let tier = if is_exemplar(repo_url, exemplars) {
        0u8
    } else {
        1u8
    };
    // Invert stars/confidence so sort ascending on this tuple = exemplars, then
    // highest stars, then highest confidence first.
    let star_key = u64::MAX.saturating_sub(stars);
    let conf_key = ((1.0 - confidence.clamp(0.0, 1.0)) * 1_000_000.0) as u64;
    (tier, star_key, conf_key)
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_and_dedupes() {
        let raw = r#"
# headline
oven-sh/bun
https://github.com/uutils/coreutils/
oven-sh/bun
"#;
        let slugs = parse_lines(raw);
        assert_eq!(slugs, vec!["oven-sh/bun", "uutils/coreutils"]);
    }

    #[test]
    fn exemplar_sorts_before_others() {
        let set: HashSet<_> = ["oven-sh/bun".into()].into_iter().collect();
        let a = backfill_priority("https://github.com/oven-sh/bun", 90_000, 0.5, &set);
        let b = backfill_priority("https://github.com/foo/bar", 90_000, 0.9, &set);
        assert!(a < b);
    }
}

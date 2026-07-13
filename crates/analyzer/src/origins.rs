//! Curated predecessor labels for known replacement projects (`vs …` chips).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::exemplars::normalize_slug;

const RAW: &str = include_str!("../../../data/known-origins.txt");

fn map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut out = HashMap::new();
        for line in RAW.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((slug, origin)) = line.split_once('|') else {
                continue;
            };
            let slug = normalize_slug(slug);
            let origin = origin.trim();
            if slug.is_empty() || origin.is_empty() {
                continue;
            }
            out.insert(slug, origin.to_string());
        }
        out
    })
}

/// Look up a curated origin label by `owner/repo` or full GitHub URL.
pub fn lookup(repo: &str) -> Option<String> {
    map().get(&normalize_slug(repo)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_known_replacement() {
        assert_eq!(lookup("denoland/deno").as_deref(), Some("Node.js"));
        assert_eq!(
            lookup("https://github.com/sharkdp/fd/").as_deref(),
            Some("find")
        );
    }
}

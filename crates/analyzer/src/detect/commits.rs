//! Language-composition analysis.
//!
//! GitHub's `languages` endpoint reports how many bytes of each language a
//! repository contains. We use that byte breakdown as a cheap, reliable proxy
//! for "is this actually a Rust codebase, and what did it displace?" without
//! having to walk commit history. A repo that talks about a rewrite *and* is
//! now mostly Rust is far more likely to be a genuine rewrite than talk alone.

use crate::types::Candidate;

/// Minimum share of total bytes a non-Rust language must hold to count as the
/// displaced original. Below this threshold the language is incidental
/// (config, scripts, small tooling) and must not drive classification or the
/// shift badge.
pub const MIN_ORIGINAL_LANGUAGE_PCT: f64 = 12.0;

/// Languages that are never evidence of a prior shipping application
/// (build files, shell glue, docs tooling). Matching these as "original"
/// produced false rewrites (e.g. Makefile→Rust tutorials).
const NOISE_LANGUAGES: &[&str] = &[
    "makefile",
    "cmake",
    "shell",
    "dockerfile",
    "nix",
    "html",
    "css",
    "scss",
    "less",
    "markdown",
    "tex",
    "batchfile",
    "powershell",
    "procfile",
    "jupyter notebook",
];

/// True when `lang` is a real application language that could have hosted a
/// shipping product before a Rust migration (C, JS, Go, … — not Makefile).
pub fn is_real_application_language(lang: &str) -> bool {
    !NOISE_LANGUAGES
        .iter()
        .any(|n| lang.eq_ignore_ascii_case(n))
}

/// The outcome of analyzing a repository's language byte-breakdown.
#[derive(Debug, Clone, Default)]
pub struct LanguageAnalysis {
    /// Rust share of the codebase in the range 0.0 - 100.0.
    pub rust_percentage: f64,
    /// Largest non-Rust language, treated as the displaced/original language.
    pub original_language: Option<String>,
    /// True when Rust is the single largest language in the repo.
    pub rust_is_primary: bool,
}

/// Compute the Rust share and infer the displaced language for a candidate.
pub fn analyze(candidate: &Candidate) -> LanguageAnalysis {
    let total: u64 = candidate.languages.iter().map(|(_, bytes)| *bytes).sum();
    if total == 0 {
        return LanguageAnalysis::default();
    }

    let rust_bytes = candidate
        .languages
        .iter()
        .find(|(lang, _)| lang.eq_ignore_ascii_case("rust"))
        .map(|(_, bytes)| *bytes)
        .unwrap_or(0);

    let rust_percentage = (rust_bytes as f64 / total as f64) * 100.0;

    // Languages arrive sorted by byte count (descending) from the client.
    let top_language = candidate.languages.first().map(|(lang, _)| lang.as_str());
    let rust_is_primary = matches!(top_language, Some(l) if l.eq_ignore_ascii_case("rust"));

    // The displaced language is the largest non-Rust language, but only when
    // it represents a meaningful share of the repo (not a sliver of Nix/shell).
    let original_language = candidate
        .languages
        .iter()
        .filter(|(lang, _)| !lang.eq_ignore_ascii_case("rust"))
        .filter(|(lang, _)| is_real_application_language(lang))
        .max_by_key(|(_, bytes)| *bytes)
        .and_then(|(lang, bytes)| {
            let share = (*bytes as f64 / total as f64) * 100.0;
            if share >= MIN_ORIGINAL_LANGUAGE_PCT {
                Some(lang.clone())
            } else {
                None
            }
        })
        .filter(|lang| !lang.eq_ignore_ascii_case("rust"));

    LanguageAnalysis {
        rust_percentage,
        original_language,
        rust_is_primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Candidate;

    fn langs(pairs: &[(&str, u64)]) -> Vec<(String, u64)> {
        pairs
            .iter()
            .map(|(lang, bytes)| (lang.to_string(), *bytes))
            .collect()
    }

    #[test]
    fn helix_like_all_rust_has_no_original_language() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 980_000), ("Nix", 20_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert!((analysis.rust_percentage - 98.0).abs() < 0.01);
        assert!(analysis.original_language.is_none());
        assert!(analysis.rust_is_primary);
    }

    #[test]
    fn coreutils_like_substantial_c_is_original_language() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 600_000), ("C", 400_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert_eq!(analysis.original_language.as_deref(), Some("C"));
    }

    #[test]
    fn makefile_is_not_original_language() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 600_000), ("Makefile", 400_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert!(analysis.original_language.is_none());
    }

    #[test]
    fn shell_is_not_original_language() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 600_000), ("Shell", 400_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert!(analysis.original_language.is_none());
    }

    #[test]
    fn rust_only_repo_has_no_original_language() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 1_000_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert!(analysis.original_language.is_none());
    }

    #[test]
    fn original_language_is_never_rust() {
        let candidate = Candidate {
            languages: langs(&[("Rust", 1_000_000)]),
            ..Default::default()
        };
        let analysis = analyze(&candidate);
        assert_ne!(analysis.original_language.as_deref(), Some("Rust"));
    }
}

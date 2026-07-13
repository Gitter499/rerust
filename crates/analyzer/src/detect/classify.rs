//! Structural-provenance classification: `Rewrite` / `Replacement` / `Neither`.
//!
//! - **Rewrite**: same shipping product migrated to Rust (identity + rising-Rust history).
//! - **Replacement**: new Rust tool / third-party port competing with an external product.
//! - **Neither**: tutorials, toys, API shims, bare phrase bait.

use crate::detect::commits::LanguageAnalysis;
use crate::detect::transitions::HistoryAnalysis;
use crate::types::Candidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Rewrite,
    Replacement,
    Neither,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Rewrite => "rewrite",
            ProjectKind::Replacement => "replacement",
            ProjectKind::Neither => "neither",
        }
    }
}

// --- Phrase tables -----------------------------------------------------------

const IN_PLACE: &[&str] = &[
    "rewrite of", "rewritten in rust", "rewrite in rust", "rewritten from",
    "translated to rust", "translation of", "now written in rust", "now in rust",
    "migrated to rust", "migration to rust", "migrating to rust",
];

const EXTERNAL_PORT: &[&str] = &[
    "port of", "rust port of", "rust port", "ported to rust", "porting to rust",
    "reimplementation of", "reimplementation in rust", "reimplemented in rust",
    "reimplementing", "pure-rust port", "pure rust port",
];

const COMPETITOR: &[&str] = &[
    "drop-in replacement", "drop in replacement", "alternative to", "alternative for",
    "replacement for", "replacement of", "replaces ", "successor to", "successor of",
    "in place of", "instead of", "compatible with", "clone of",
];

const ORIGIN_MARKERS: &[&str] = &[
    "reimplementation in rust of", "reimplementation of", "rust port of", "port of",
    "rewrite of", "rewritten from", "translation of", "clone of", "fork of",
    "drop-in replacement for", "drop-in replacement of", "drop in replacement for",
    "drop in replacement of", "drop-in replacements for", "drop-in replacements of",
    "replacement for", "replacement of", "alternative to", "alternative for",
    "successor to", "inspired by", "based on", "compatible with",
];

const ORIGIN_STOPS: &[&str] = &[
    ",", ".", ";", ":", "(", ")", "`", "\"", "/", "!", "?", "\n", " — ", " - ",
    " using ", " written ", " built ", " with ", " that ", " which ",
    " in rust", " and ", " for ",
    " repo ", " mentions ", " describes ", " positions ", " is a ",
];

const API_MARKERS: &[&str] = &[
    "std::", "::", "#[derive", "derive(", "assert_eq", "assert_ne",
    "rust's", "rust channel", "rust crate",
];

const IDENTITY_STOPWORDS: &[&str] = &[
    "rust", "rs", "next", "gen", "new", "the", "and", "for", "with", "from",
    "port", "rewrite", "rewritten", "replacement", "drop", "in", "of", "a", "an",
    "to", "in", "engine", "lib", "crate", "sdk", "cli", "app", "bot", "tool",
];

fn any_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| text.contains(p))
}

/// "rewrite <tool> in rust" / "port <tool> to rust" with a named gap.
fn named_migration_verb(text: &str) -> bool {
    let named_gap = |gap: &str| {
        let gap = gap.trim();
        !gap.is_empty() && gap != "in" && gap != "to"
    };
    for (verb, end) in [
        ("rewrite ", "in rust"),
        ("rewriting ", "in rust"),
        ("rewrote ", "in rust"),
        ("port ", "to rust"),
        ("porting ", "to rust"),
        ("ported ", "to rust"),
        ("migrate ", "to rust"),
        ("migrating ", "to rust"),
    ] {
        if let Some(i) = text.find(verb) {
            let after = &text[i + verb.len()..];
            if let Some(j) = after.find(end) {
                if named_gap(&after[..j]) {
                    return true;
                }
            }
        }
    }
    false
}

fn in_place(text: &str) -> bool {
    any_phrase(text, IN_PLACE) || named_migration_verb(text)
}

fn external_port(text: &str) -> bool {
    any_phrase(text, EXTERNAL_PORT)
}

fn competitor(text: &str) -> bool {
    any_phrase(text, COMPETITOR)
}

fn targeted_migration(text: &str) -> bool {
    any_phrase(
        text,
        &[
            "rewrite of", "rewritten from", "port of", "rust port of",
            "reimplementation of", "reimplementation in rust of", "translation of",
        ],
    ) || named_migration_verb(text)
}

fn combined_text(candidate: &Candidate) -> String {
    let mut text = candidate.description.as_deref().unwrap_or("").to_lowercase();
    for s in &candidate.signals {
        text.push(' ');
        text.push_str(&s.detail.to_lowercase());
    }
    text
}

// --- Named origin ------------------------------------------------------------

pub fn extract_named_origin(text: &str) -> Option<String> {
    for marker in ORIGIN_MARKERS {
        if let Some(i) = text.find(marker) {
            if let Some(name) = clean_origin(&text[i + marker.len()..]) {
                return Some(name);
            }
        }
    }
    None
}

fn clean_origin(after: &str) -> Option<String> {
    let after = after.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ':' | '-')
    });
    let mut end = after.len();
    for stop in ORIGIN_STOPS {
        if let Some(i) = after.find(stop) {
            end = end.min(i);
        }
    }
    let mut name = after[..end].trim().to_string();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = name.strip_prefix(article) {
            name = rest.to_string();
        }
    }
    let name: String = name.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn is_generic_origin_signal(detail: &str) -> bool {
    let d = detail.to_lowercase();
    d.contains("named project")
        || d.starts_with("repo describes")
        || d.starts_with("repo mentions")
        || d.starts_with("repo positions")
        || d.contains("repo describes a")
        || d.contains("repo mentions a")
        || d.contains("repo positions itself")
}

fn extract_origin_from_candidate(candidate: &Candidate) -> Option<String> {
    if let Some(desc) = candidate.description.as_deref() {
        if let Some(origin) = extract_named_origin(&desc.to_lowercase()) {
            return Some(origin);
        }
    }
    for s in &candidate.signals {
        if is_generic_origin_signal(&s.detail) {
            continue;
        }
        if let Some(origin) = extract_named_origin(&s.detail.to_lowercase()) {
            return Some(origin);
        }
    }
    None
}

pub fn named_origin(candidate: &Candidate) -> Option<String> {
    extract_origin_from_candidate(candidate)
}

fn rust_api(text: &str) -> bool {
    API_MARKERS.iter().any(|m| text.contains(m))
}

/// True when the repo advertises replacing/migrating a specific existing tool.
pub fn has_strong_rewrite_signal(candidate: &Candidate) -> bool {
    let matches = |text: &str| competitor(text) || targeted_migration(text) || in_place(text);
    let description = candidate.description.as_deref().unwrap_or("").to_lowercase();
    if matches(&description) {
        return true;
    }
    candidate.signals.iter().any(|s| {
        (s.kind == "repo-search" || s.kind == "pull-request") && matches(&s.detail.to_lowercase())
    })
}

// --- Identity continuity -----------------------------------------------------

fn significant_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .filter(|t| !IDENTITY_STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

fn product_slug(candidate: &Candidate) -> String {
    let name = candidate
        .full_name
        .rsplit('/')
        .next()
        .unwrap_or(&candidate.full_name);
    let lower = name.to_lowercase();
    for suffix in ["-rs", "-rust", "_rs", "_rust", "-next", "-ng", "-next-gen"] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            if stripped.len() >= 3 {
                return stripped.to_string();
            }
        }
    }
    lower
}

fn compact_alnum(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Prefix/equality compact match — never `contains`, which false-positives clones.
fn compact_identity(a: &str, b: &str, min: usize) -> bool {
    let ca = compact_alnum(a);
    let cb = compact_alnum(b);
    ca.len() >= min
        && cb.len() >= min
        && (ca == cb || ca.starts_with(&cb) || cb.starts_with(&ca))
}

fn tokens_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|t| {
        b.iter().any(|u| {
            if t == u {
                return true;
            }
            // Substantial stems only; require prefix relation (not PathPicker ⊂ FastPathPicker).
            t.len() >= 6 && u.len() >= 6 && (u.starts_with(t.as_str()) || t.starts_with(u.as_str()))
        })
    })
}

fn product_before_rewrite_phrase(text: &str) -> Option<String> {
    for marker in [
        " rewritten in rust",
        " rewrite in rust",
        " rewritten from",
        " migrated to rust",
    ] {
        let Some(i) = text.find(marker) else { continue };
        let words: Vec<&str> = text[..i]
            .split_whitespace()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let mut start = 0;
        while start < words.len() {
            let w = words[start].trim_matches(|c: char| !c.is_alphanumeric());
            if IDENTITY_STOPWORDS.contains(&w)
                || matches!(
                    w,
                    "high" | "performance" | "lightning" | "fast" | "experimental"
                        | "next-generation" | "next" | "generation"
                )
            {
                start += 1;
                continue;
            }
            break;
        }
        if start >= words.len() {
            continue;
        }
        let phrase = words[start..]
            .iter()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !phrase.is_empty() {
            return Some(phrase);
        }
    }
    None
}

fn names_match_product(name: &str, slug: &str, slug_tokens: &[String], owner_tokens: &[String]) -> bool {
    let name_tokens = significant_tokens(name);
    tokens_overlap(&name_tokens, slug_tokens)
        || tokens_overlap(&name_tokens, owner_tokens)
        || name.replace(' ', "-").contains(slug)
        || slug.contains(&name.replace(' ', "-"))
        || compact_identity(name, slug, 5)
}

/// True when this repo *is* the shipping product being migrated (not a third-party port).
pub fn has_identity_continuity(candidate: &Candidate) -> bool {
    let text = combined_text(candidate);
    let slug = product_slug(candidate);
    let slug_tokens = significant_tokens(&slug);
    let owner = candidate
        .full_name
        .split('/')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let owner_tokens = significant_tokens(&owner);

    if let Some(product) = product_before_rewrite_phrase(&text) {
        if names_match_product(&product, &slug, &slug_tokens, &owner_tokens) {
            return true;
        }
    }

    if let Some(origin) = named_origin(candidate) {
        let origin_tokens = significant_tokens(&origin);
        if tokens_overlap(&origin_tokens, &slug_tokens)
            || tokens_overlap(&origin_tokens, &owner_tokens)
            || compact_identity(&origin, &slug, 6)
        {
            return true;
        }
    }

    for marker in ["rewrite of ", "rewritten from "] {
        if let Some(i) = text.find(marker) {
            if let Some(name) = clean_origin(&text[i + marker.len()..]) {
                if tokens_overlap(&significant_tokens(&name), &slug_tokens) {
                    return true;
                }
            }
        }
    }

    candidate.signals.iter().any(|s| {
        (s.kind == "pull-request" || s.kind == "issue")
            && signal_is_product_migration(&s.detail, &slug, &slug_tokens, &owner_tokens)
    })
}

pub(crate) fn signal_title_body(detail: &str) -> &str {
    detail
        .split_once(": ")
        .map(|(_, t)| t)
        .filter(|t| !t.is_empty())
        .unwrap_or(detail)
}

fn signal_is_migration_discussion(detail: &str) -> bool {
    let title = signal_title_body(detail).to_lowercase();
    title.contains("rust")
        && (title.contains("rewrite")
            || title.contains("rewriting")
            || title.contains("rewritten")
            || title.contains("port to")
            || title.contains("ported to")
            || title.contains("migrate")
            || title.contains("migrating")
            || title.contains("migrated"))
}

fn signal_is_product_migration(
    detail: &str,
    slug: &str,
    slug_tokens: &[String],
    owner_tokens: &[String],
) -> bool {
    if !signal_is_migration_discussion(detail) {
        return false;
    }
    let title = signal_title_body(detail).to_lowercase();
    let title_tokens = significant_tokens(&title);
    if tokens_overlap(&title_tokens, slug_tokens) || tokens_overlap(&title_tokens, owner_tokens) {
        return true;
    }
    let compact_slug = compact_alnum(slug);
    if compact_slug.len() >= 4 && compact_alnum(&title).contains(&compact_slug) {
        return true;
    }
    // Exclude RIIR meme / wishlist phrasing.
    if title.contains("rewrite it in")
        || title.contains("rewritten in rust?")
        || title.contains("riir")
    {
        return false;
    }
    title.contains("rewrite this")
        || title.contains("rewriting this")
        || title.contains("rewrite the codebase")
        || title.contains("rewrite our")
        || title.contains("migrate this")
        || title.contains("migrate the codebase")
}

fn real_displaced_language(analysis: &LanguageAnalysis) -> bool {
    analysis
        .original_language
        .as_deref()
        .is_some_and(crate::detect::commits::is_real_application_language)
}

fn strong_history(history: &HistoryAnalysis) -> bool {
    history.strong_transition
        && history
            .from_language
            .as_deref()
            .is_some_and(crate::detect::commits::is_real_application_language)
}

/// True when a PR/issue signal discusses a real language migration (not a wishlist meme).
fn has_migration_pr(candidate: &Candidate) -> bool {
    candidate.signals.iter().any(|s| {
        (s.kind == "pull-request" || s.kind == "issue")
            && signal_is_migration_discussion(&s.detail)
            && !{
                let title = signal_title_body(&s.detail).to_lowercase();
                title.contains("rewrite it in")
                    || title.contains("rewritten in rust?")
                    || title.contains("riir")
            }
    })
}

/// Classify under identity-continuity provenance.
///
/// `Rewrite` ⟺ identity ∧ (commit-proven rising-Rust history **or** a product
/// migration PR). History alone can miss monorepo transitions (e.g. Bun); a
/// merged "Rewrite X in Rust" / "migrate from Zig to Rust" PR on the product
/// itself is enough when identity already holds.
/// Everything else with migration/competitor framing → `Replacement`.
/// API shims / bare bait → `Neither`.
pub fn classify(
    candidate: &Candidate,
    analysis: &LanguageAnalysis,
    history: &HistoryAnalysis,
) -> ProjectKind {
    let text = combined_text(candidate);
    let origin = named_origin(candidate);
    let identity = has_identity_continuity(candidate);
    let has_in_place = in_place(&text);
    let has_external = external_port(&text);
    let has_competitor = competitor(&text);
    let real_lang = real_displaced_language(analysis);
    let hist = strong_history(history);
    let migration = has_in_place || has_external || has_competitor;
    let migration_pr = has_migration_pr(candidate);

    if rust_api(&text) || origin.as_deref().is_some_and(rust_api) {
        return ProjectKind::Neither;
    }

    // Same product + (proven history **or** product migration PR).
    if identity && (hist || migration_pr) {
        return ProjectKind::Rewrite;
    }

    // Competitor / third-party port / same-product claim without history.
    if (has_competitor || has_external)
        && !identity
        && (origin.is_some() || real_lang || has_competitor)
    {
        return ProjectKind::Replacement;
    }
    if hist && migration && !identity {
        return ProjectKind::Replacement;
    }
    if identity && !hist && !migration_pr && (has_in_place || has_external) {
        return ProjectKind::Replacement;
    }
    if (has_external || has_competitor) && (origin.is_some() || real_lang || has_competitor) {
        return ProjectKind::Replacement;
    }
    if has_in_place && (real_lang || targeted_migration(&text)) && !identity {
        return ProjectKind::Replacement;
    }

    ProjectKind::Neither
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::transitions::HistoryAnalysis;
    use crate::types::Signal;

    fn no_history() -> HistoryAnalysis {
        HistoryAnalysis::default()
    }

    fn rising_rust_history(from: &str) -> HistoryAnalysis {
        HistoryAnalysis {
            strong_transition: true,
            rust_pct_before: Some(8.0),
            rust_pct_after: Some(82.0),
            transition_magnitude: Some(74.0),
            from_language: Some(from.into()),
            total_commits: 500,
            ..Default::default()
        }
    }

    fn candidate(description: &str, signals: Vec<Signal>) -> Candidate {
        Candidate {
            description: Some(description.to_string()),
            signals,
            ..Default::default()
        }
    }

    fn named(full_name: &str, description: &str) -> Candidate {
        Candidate {
            full_name: full_name.into(),
            description: Some(description.into()),
            ..Default::default()
        }
    }

    fn repo_signal(detail: &str) -> Signal {
        Signal {
            kind: "repo-search".into(),
            detail: detail.into(),
            url: "https://example".into(),
        }
    }

    fn analysis(rust_pct: f64, original: Option<&str>, primary: bool) -> LanguageAnalysis {
        LanguageAnalysis {
            rust_percentage: rust_pct,
            original_language: original.map(str::to_string),
            rust_is_primary: primary,
        }
    }

    #[test]
    fn script_kit_same_product_with_history_is_rewrite() {
        let c = named(
            "johnlindquist/script-kit-next",
            "Script Kit rewritten in Rust using GPUI",
        );
        let a = analysis(85.0, Some("TypeScript"), true);
        assert!(has_identity_continuity(&c));
        assert_eq!(
            classify(&c, &a, &rising_rust_history("JavaScript")),
            ProjectKind::Rewrite
        );
    }

    #[test]
    fn falkordb_without_commit_history_is_replacement() {
        let c = named(
            "FalkorDB/falkordb-rs-next-gen",
            "The next-generation FalkorDB engine rewritten in Rust.",
        );
        let a = analysis(44.0, Some("Python"), true);
        assert!(has_identity_continuity(&c));
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn falkordb_with_commit_history_is_rewrite() {
        let c = named(
            "FalkorDB/falkordb-rs-next-gen",
            "The next-generation FalkorDB engine rewritten in Rust.",
        );
        let a = analysis(44.0, Some("Python"), true);
        assert_eq!(
            classify(&c, &a, &rising_rust_history("Python")),
            ProjectKind::Rewrite
        );
    }

    #[test]
    fn always_rust_same_product_claim_is_replacement_without_history() {
        let c = named(
            "BurntSushi/ripgrep",
            "ripgrep rewritten in Rust — a line-oriented search tool",
        );
        let a = analysis(98.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn coreutils_style_rewrite_of_self_is_rewrite() {
        let c = named(
            "uutils/coreutils",
            "Cross-platform Rust rewrite of the GNU coreutils",
        );
        let a = analysis(60.0, Some("C"), true);
        assert!(has_identity_continuity(&c));
        assert_eq!(
            classify(&c, &a, &rising_rust_history("C")),
            ProjectKind::Rewrite
        );
    }

    #[test]
    fn wat_dropin_for_wakatime_is_replacement_even_with_history() {
        let c = named(
            "mzhang28/wat",
            "self-hosted drop-in replacement for wakatime, a time-tracking service",
        );
        let a = analysis(94.0, Some("Go"), true);
        assert!(!has_identity_continuity(&c));
        assert_eq!(
            classify(&c, &a, &rising_rust_history("Go")),
            ProjectKind::Replacement
        );
    }

    #[test]
    fn cj_dropin_for_jc_is_replacement() {
        let c = named(
            "zhongweili/cj",
            "A fast, drop-in replacement for jc, rewritten in Rust.",
        );
        let a = analysis(99.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn miemietron_dropin_for_mihomo_is_replacement() {
        let c = named(
            "xwings/miemietron",
            "Drop-in replacement for mihomo (Clash Meta), rewritten in Rust.",
        );
        let a = analysis(99.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn ruannoy_port_is_replacement() {
        let c = named(
            "hanabi1224/RuAnnoy",
            "Rust port of annoy (https://github.com/spotify/annoy)",
        );
        let a = analysis(64.0, Some("C#"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn pounce_port_is_replacement() {
        let c = named("jkitchin/pounce", "Pure-Rust port of Ipopt (NLP/IPM solver)");
        let a = analysis(66.0, Some("Python"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn rvface_does_not_get_identity_from_face_substring() {
        let c = named(
            "ruvnet/rvFACE",
            "Rust port of Faceplugin's open-source Face-Recognition-SDK.",
        );
        assert!(!has_identity_continuity(&c));
        let a = analysis(60.0, Some("TypeScript"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn fastpathpicker_clone_is_replacement() {
        let c = named(
            "devinjeon/FastPathPicker",
            "fpp2: Facebook PathPicker (fpp) rewritten in Rust — drop-in replacement.",
        );
        let a = analysis(56.0, Some("Shell"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn pzoom_port_of_psalm_is_replacement() {
        let c = named(
            "muglug/pzoom",
            "A fast experimental PHP static analyzer written in Rust, a port of Psalm",
        );
        let a = analysis(76.0, Some("PHP"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn ripgrep_style_is_replacement() {
        let c = candidate(
            "a faster alternative to grep",
            vec![repo_signal("repo is a drop-in replacement written in Rust")],
        );
        let a = analysis(98.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn yjs_port_without_identity_is_replacement() {
        let c = named("y-crdt/y-crdt", "Rust port of Yjs");
        let a = analysis(76.0, Some("C++"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn wplusplus_toy_without_identity_is_neither_or_replacement() {
        let c = named(
            "sinisterMage/WPlusPlus",
            "A Python-style scripting language rewritten in Rust & LLVM.",
        );
        let a = analysis(64.0, Some("C#"), true);
        assert!(!has_identity_continuity(&c));
        let kind = classify(&c, &a, &rising_rust_history("C#"));
        assert!(
            kind == ProjectKind::Replacement || kind == ProjectKind::Neither,
            "got {kind:?}"
        );
    }

    #[test]
    fn linux_driver_tutorial_makefile_is_not_rewrite() {
        let c = named(
            "d0u9/Linux-Device-Driver-Rust",
            "A try to follow the rust port in Linux kernel in driver development.",
        );
        let a = analysis(62.0, Some("Makefile"), true);
        let hist = HistoryAnalysis {
            strong_transition: true,
            rust_pct_before: Some(33.0),
            rust_pct_after: Some(60.0),
            transition_magnitude: Some(27.0),
            from_language: Some("Makefile".into()),
            total_commits: 31,
            ..Default::default()
        };
        assert_ne!(classify(&c, &a, &hist), ProjectKind::Rewrite);
    }

    #[test]
    fn rust_pretty_assertions_is_neither() {
        let c = candidate(
            "Overwrite `assert_eq!` with a drop-in replacement, adding a colorful diff.",
            vec![],
        );
        let a = analysis(97.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Neither);
    }

    #[test]
    fn helix_style_bare_rewrite_is_neither() {
        let c = candidate(
            "A post-modern modal text editor.",
            vec![repo_signal("repo mentions \"rewrite in Rust\"")],
        );
        let a = analysis(98.0, None, true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Neither);
    }

    #[test]
    fn extracts_named_origin() {
        assert_eq!(extract_named_origin("rust port of yjs").as_deref(), Some("yjs"));
        assert_eq!(
            extract_named_origin("drop-in replacement for gnu sed, written in rust").as_deref(),
            Some("gnu sed")
        );
    }

    #[test]
    fn coreutils_origin_ignores_signal_boilerplate() {
        let c = candidate(
            "Cross-platform Rust rewrite of the GNU coreutils",
            vec![repo_signal("repo describes a rewrite of a named project")],
        );
        assert_eq!(named_origin(&c).as_deref(), Some("gnu coreutils"));
    }

    #[test]
    fn origin_stops_at_repo_boilerplate() {
        assert_eq!(
            extract_named_origin(
                "cross-platform rust rewrite of the gnu coreutils repo describes a rewrite of a named project"
            )
            .as_deref(),
            Some("gnu coreutils")
        );
    }

    #[test]
    fn issue_on_product_repo_with_migration_pr_is_rewrite() {
        let mut c = named("oven-sh/bun", "Incredibly fast JavaScript runtime");
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about rewriting in Rust: Rewrite Bun in Rust".into(),
            url: "https://github.com/oven-sh/bun/pull/30412".into(),
        });
        let a = analysis(70.0, Some("Zig"), true);
        assert!(has_identity_continuity(&c));
        // Product migration PR is enough even when commit history is weak/empty.
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Rewrite);
        assert_eq!(
            classify(&c, &a, &rising_rust_history("Zig")),
            ProjectKind::Rewrite
        );
    }

    #[test]
    fn bun_multiple_migration_prs_still_rewrite() {
        let mut c = named("oven-sh/bun", "Incredibly fast JavaScript runtime");
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about rewriting in Rust: Rewrite Bun in Rust".into(),
            url: "https://github.com/oven-sh/bun/pull/30412".into(),
        });
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about migrating to Rust: refactor: migrate from zig to rust"
                .into(),
            url: "https://github.com/oven-sh/bun/pull/30698".into(),
        });
        let a = analysis(70.0, Some("Zig"), true);
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Rewrite);
    }

    #[test]
    fn wishlist_issue_alone_is_not_identity() {
        let mut c = named(
            "EnterpriseQualityCoding/FizzBuzzEnterpriseEdition",
            "FizzBuzz Enterprise Edition",
        );
        c.signals.push(Signal {
            kind: "issue".into(),
            detail: "issue titled about rewriting in Rust: Rewrite it in Rust".into(),
            url: "https://github.com/EnterpriseQualityCoding/FizzBuzzEnterpriseEdition/issues/1"
                .into(),
        });
        assert!(!has_identity_continuity(&c));
        let a = analysis(0.0, Some("Java"), false);
        assert_ne!(classify(&c, &a, &no_history()), ProjectKind::Rewrite);
    }

    #[test]
    fn subsystem_port_pr_without_product_name_is_not_rewrite() {
        let mut c = named("some-org/vogon-runtime", "A runtime");
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about porting to Rust: Migrate lint check to Rust".into(),
            url: "https://github.com/some-org/vogon-runtime/pull/1".into(),
        });
        assert!(!has_identity_continuity(&c));
        let a = analysis(80.0, Some("C++"), true);
        assert_ne!(classify(&c, &a, &no_history()), ProjectKind::Rewrite);
    }

    #[test]
    fn identity_detects_script_kit_and_rejects_wakatime_clone() {
        assert!(has_identity_continuity(&named(
            "johnlindquist/script-kit-next",
            "Script Kit rewritten in Rust"
        )));
        assert!(!has_identity_continuity(&named(
            "mzhang28/wat",
            "drop-in replacement for wakatime"
        )));
    }
}

//! Structural-provenance classification: `Rewrite` / `Replacement` / `Neither`.
//!
//! Semantic intent (Rewrite is the primary focus):
//!
//!   * **Rewrite**: the *same shipping product* that previously ran in another
//!     language was migrated to Rust for performance (Bun, Astro compiler,
//!     React compiler, uutils/coreutils). Requires **identity continuity** —
//!     this repo *is* that product — plus evidence of a real language migration.
//!
//!   * **Replacement**: a *new* Rust project that reimplements or competes with
//!     an external tool (ripgrep vs grep, RuAnnoy vs annoy, cj vs jc). Includes
//!     third-party "Rust port of X" when X is someone else's product.
//!
//!   * **Neither**: tutorials, toys, API shims, phrase bait with no provenance.
//!
//! Phrase lists alone are not enough: `"rewritten in Rust"` + rising history can
//! describe a greenfield clone. Identity continuity is the gate for Rewrite.

use crate::detect::commits::LanguageAnalysis;
use crate::detect::transitions::HistoryAnalysis;
use crate::types::Candidate;

/// How a Rust project relates to the tool it descends from or competes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// Same shipping product migrated to Rust. Primary focus.
    Rewrite,
    /// New independent Rust tool / third-party port competing with an external one.
    Replacement,
    /// No genuine cross-language product story. Dropped from the site.
    Neither,
}

impl ProjectKind {
    /// Stable string form stored in SQLite and serialized to the frontend.
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Rewrite => "rewrite",
            ProjectKind::Replacement => "replacement",
            ProjectKind::Neither => "neither",
        }
    }
}

/// Phrases implying the *same project's own codebase* was migrated to Rust.
const IN_PLACE_MIGRATION_PHRASES: &[&str] = &[
    "rewrite of",
    "rewritten in rust",
    "rewrite in rust",
    "rewritten from",
    "translated to rust",
    "translation of",
    "now written in rust",
    "now in rust",
    "migrated to rust",
    "migration to rust",
    "migrating to rust",
];

/// Phrases implying a *new Rust repo* reimplements an external predecessor.
const EXTERNAL_PORT_PHRASES: &[&str] = &[
    "port of",
    "rust port of",
    "rust port",
    "ported to rust",
    "porting to rust",
    "reimplementation of",
    "reimplementation in rust",
    "reimplemented in rust",
    "reimplementing",
    "pure-rust port",
    "pure rust port",
];

/// Migration phrases that name a displaced source ("… of X").
const TARGETED_MIGRATION_PHRASES: &[&str] = &[
    "rewrite of",
    "rewritten from",
    "port of",
    "rust port of",
    "reimplementation of",
    "reimplementation in rust of",
    "translation of",
];

/// Competitor / stand-in framing → Replacement unless identity continuity holds.
const COMPETITOR_TERMS: &[&str] = &[
    "drop-in replacement",
    "drop in replacement",
    "alternative to",
    "alternative for",
    "replacement for",
    "replacement of",
    "replaces ",
    "successor to",
    "successor of",
    "in place of",
    "instead of",
    "compatible with",
    "clone of",
];

const ORIGIN_MARKERS: &[&str] = &[
    "reimplementation in rust of",
    "reimplementation of",
    "rust port of",
    "port of",
    "rewrite of",
    "rewritten from",
    "translation of",
    "clone of",
    "fork of",
    "drop-in replacement for",
    "drop-in replacement of",
    "drop in replacement for",
    "drop in replacement of",
    "drop-in replacements for",
    "drop-in replacements of",
    "replacement for",
    "replacement of",
    "alternative to",
    "alternative for",
    "successor to",
    "inspired by",
    "based on",
    "compatible with",
];

const ORIGIN_STOPS: &[&str] = &[
    ",", ".", ";", ":", "(", ")", "`", "\"", "/", "!", "?", "\n", " — ", " - ",
    " using ", " written ", " built ", " with ", " that ", " which ",
    " in rust", " and ", " for ",
    // Discovery-signal boilerplate appended after description in combined text.
    " repo ", " mentions ", " describes ", " positions ", " is a ",
];

const GLOBAL_API_MARKERS: &[&str] = &["std::", "::", "#[derive", "derive(", "assert_eq", "assert_ne"];

const TARGET_API_MARKERS: &[&str] = &[
    "rust's", "rust channel", "rust crate", "std::", "#[derive", "assert_eq",
];

/// Tokens too generic to prove product identity.
const IDENTITY_STOPWORDS: &[&str] = &[
    "rust", "rs", "next", "gen", "new", "the", "and", "for", "with", "from",
    "port", "rewrite", "rewritten", "replacement", "drop", "in", "of", "a", "an",
    "to", "in", "engine", "lib", "crate", "sdk", "cli", "app", "bot", "tool",
];

fn has_named_migration_verb(text: &str) -> bool {
    let gap_names_a_tool = |gap: &str| {
        let gap = gap.trim();
        !gap.is_empty() && gap != "in" && gap != "to"
    };
    for verb in ["rewrite ", "rewriting ", "rewrote "] {
        if let Some(i) = text.find(verb) {
            let after = &text[i + verb.len()..];
            if let Some(j) = after.find("in rust") {
                if gap_names_a_tool(&after[..j]) {
                    return true;
                }
            }
        }
    }
    for verb in ["port ", "porting ", "ported ", "migrate ", "migrating "] {
        if let Some(i) = text.find(verb) {
            let after = &text[i + verb.len()..];
            if let Some(j) = after.find("to rust") {
                if gap_names_a_tool(&after[..j]) {
                    return true;
                }
            }
        }
    }
    false
}

fn has_in_place_migration_wording(text: &str) -> bool {
    IN_PLACE_MIGRATION_PHRASES
        .iter()
        .any(|p| text.contains(p))
        || has_named_migration_verb(text)
}

fn has_external_port_wording(text: &str) -> bool {
    EXTERNAL_PORT_PHRASES.iter().any(|p| text.contains(p))
}

fn has_targeted_migration(text: &str) -> bool {
    TARGETED_MIGRATION_PHRASES.iter().any(|p| text.contains(p)) || has_named_migration_verb(text)
}

fn has_competitor_framing(text: &str) -> bool {
    COMPETITOR_TERMS.iter().any(|t| text.contains(t))
}

fn combined_text(candidate: &Candidate) -> String {
    let mut text = candidate.description.as_deref().unwrap_or("").to_lowercase();
    for s in &candidate.signals {
        text.push(' ');
        text.push_str(&s.detail.to_lowercase());
    }
    text
}

/// Parse the specific prior project a repo displaces or reimplements.
pub fn extract_named_origin(text: &str) -> Option<String> {
    for marker in ORIGIN_MARKERS {
        if let Some(i) = text.find(marker) {
            let after = &text[i + marker.len()..];
            if let Some(name) = clean_origin(after) {
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
    let name: String = name
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Generic discovery signal text that must not pollute origin parsing.
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

/// Extract origin from description first, then non-generic signals individually.
/// Never concatenates description + signals — that caused trailing boilerplate
/// (e.g. "gnu coreutils repo describes a") to pollute the chip label.
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

/// The named origin for a candidate.
pub fn named_origin(candidate: &Candidate) -> Option<String> {
    extract_origin_from_candidate(candidate).or_else(|| {
        candidate
            .named_origin
            .as_ref()
            .filter(|o| !o.is_empty())
            .cloned()
    })
}

fn target_is_rust_api(origin: &str) -> bool {
    let o = origin.to_lowercase();
    TARGET_API_MARKERS.iter().any(|m| o.contains(m))
        || GLOBAL_API_MARKERS.iter().any(|m| o.contains(m))
}

fn mentions_rust_api(text: &str) -> bool {
    GLOBAL_API_MARKERS.iter().any(|m| text.contains(m))
}

/// True when the repo advertises replacing/migrating a *specific* existing tool.
pub fn has_strong_rewrite_signal(candidate: &Candidate) -> bool {
    let matches = |text: &str| {
        has_competitor_framing(text)
            || has_targeted_migration(text)
            || has_in_place_migration_wording(text)
    };

    let description = candidate.description.as_deref().unwrap_or("").to_lowercase();
    if matches(&description) {
        return true;
    }

    candidate.signals.iter().any(|s| {
        (s.kind == "repo-search" || s.kind == "pull-request") && matches(&s.detail.to_lowercase())
    })
}

/// Significant tokens from a product / repo name (lowercase, stopwords removed).
fn significant_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .filter(|t| !IDENTITY_STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// Repo product slug: last path segment of `owner/name`, stripped of common suffixes.
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

/// Extract a product name that appears immediately before "rewritten/rewrite … rust".
fn product_before_rewrite_phrase(text: &str) -> Option<String> {
    for marker in [
        " rewritten in rust",
        " rewrite in rust",
        " rewritten from",
        " migrated to rust",
    ] {
        if let Some(i) = text.find(marker) {
            let before = &text[..i];
            // Take the last 1–4 significant words as the product phrase.
            let words: Vec<&str> = before
                .split_whitespace()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            // Drop leading filler ("a", "the", "next-generation", …).
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
    }
    None
}

/// True when named origin / rewrite-phrase product overlaps this repo's identity.
///
/// Issue/PR signals **corroborate** migration work but do not alone prove that
/// this repo *is* the shipping product being rewritten (wishlist issues and
/// "rewrite it in Rust" memes fire constantly on unrelated repos).
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

    // 1. Product named before "rewritten in Rust" matches this repo.
    if let Some(product) = product_before_rewrite_phrase(&text) {
        let product_tokens = significant_tokens(&product);
        if tokens_overlap(&product_tokens, &slug_tokens)
            || tokens_overlap(&product_tokens, &owner_tokens)
            || product.replace(' ', "-").contains(&slug)
            || slug.contains(&product.replace(' ', "-"))
        {
            return true;
        }
        let compact_product: String = product.chars().filter(|c| c.is_alphanumeric()).collect();
        let compact_slug: String = slug.chars().filter(|c| c.is_alphanumeric()).collect();
        // Prefix/equality only — NOT slug.contains(product), which false-positives
        // clones like FastPathPicker ⊃ PathPicker.
        if compact_product.len() >= 5
            && (compact_slug == compact_product
                || compact_slug.starts_with(&compact_product)
                || compact_product.starts_with(&compact_slug))
        {
            return true;
        }
    }

    // 2. Named origin overlaps this product (same product, not external target).
    if let Some(origin) = named_origin(candidate) {
        let origin_tokens = significant_tokens(&origin);
        if tokens_overlap(&origin_tokens, &slug_tokens)
            || tokens_overlap(&origin_tokens, &owner_tokens)
        {
            return true;
        }
        let compact_origin: String = origin.chars().filter(|c| c.is_alphanumeric()).collect();
        let compact_slug: String = slug.chars().filter(|c| c.is_alphanumeric()).collect();
        if compact_origin.len() >= 6
            && compact_slug.len() >= 6
            && (compact_slug == compact_origin
                || compact_slug.starts_with(&compact_origin)
                || compact_origin.starts_with(&compact_slug))
        {
            return true;
        }
    }

    // 3. "rewrite of <this product>" style where origin marker target = slug.
    for marker in ["rewrite of ", "rewritten from "] {
        if let Some(i) = text.find(marker) {
            if let Some(name) = clean_origin(&text[i + marker.len()..]) {
                let name_tokens = significant_tokens(&name);
                if tokens_overlap(&name_tokens, &slug_tokens) {
                    return true;
                }
            }
        }
    }

    // 4. Issue/PR title references this product *and* discusses migration.
    //    Discovery labels like "PR titled about rewriting in Rust: …" are stripped
    //    so boilerplate cannot alone grant identity.
    if candidate.signals.iter().any(|s| {
        (s.kind == "pull-request" || s.kind == "issue")
            && signal_is_product_migration(&s.detail, &slug, &slug_tokens, &owner_tokens)
    }) {
        return true;
    }

    false
}

/// Strip discovery boilerplate ("PR titled …: ") and return the human title.
fn signal_title_body(detail: &str) -> &str {
    if let Some((_, title)) = detail.split_once(": ") {
        title
    } else {
        detail
    }
}

fn signal_is_migration_discussion(detail: &str) -> bool {
    let title = signal_title_body(detail).to_lowercase();
    let has_rust = title.contains("rust");
    let has_migration = title.contains("rewrite")
        || title.contains("rewriting")
        || title.contains("rewritten")
        || title.contains("port to")
        || title.contains("ported to")
        || title.contains("migrate")
        || title.contains("migrating")
        || title.contains("migrated");
    has_rust && has_migration
}

/// Migration discussion whose title also names this product (not a wishlist meme).
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
    let compact_slug: String = slug.chars().filter(|c| c.is_alphanumeric()).collect();
    if compact_slug.len() >= 4 {
        let compact_title: String = title.chars().filter(|c| c.is_alphanumeric()).collect();
        if compact_title.contains(&compact_slug) {
            return true;
        }
    }
    // Whole-product rewrite phrasing without naming an external target.
    // Exclude the classic "rewrite it in Rust" meme / wishlist phrasing.
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

fn tokens_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|t| {
        b.iter().any(|u| {
            if t == u {
                return true;
            }
            // Allow longer token to contain shorter only when the shorter is a
            // substantial stem (≥6) AND the longer does not merely *prefix-extend*
            // with a clone qualifier — actually: require mutual prefix or equality
            // for substring cases to avoid PathPicker ⊂ FastPathPicker.
            let min = 6;
            if t.len() >= min && u.len() >= min {
                u.starts_with(t.as_str()) || t.starts_with(u.as_str())
            } else {
                false
            }
        })
    })
}

/// True when displaced language is real application code (not Makefile/Shell noise).
fn real_displaced_language(analysis: &LanguageAnalysis) -> bool {
    match analysis.original_language.as_deref() {
        Some(lang) => crate::detect::commits::is_real_application_language(lang),
        None => false,
    }
}

/// Classify under identity-continuity provenance.
///
/// Rules (precision-first):
///   1. Neither — Rust API/crate shim.
///   2. Replacement — competitor / external-port framing **without** identity.
///   3. Rewrite — identity continuity **and** commit-proven rising-Rust migration
///      (`strong_history`). README/keyword claims alone are not enough: a repo
///      that was always Rust is at best a Replacement.
///   4. Replacement — external port / competitor with a named target (fallback).
///   5. Neither — everything else.
pub fn classify(
    candidate: &Candidate,
    analysis: &LanguageAnalysis,
    history: &HistoryAnalysis,
) -> ProjectKind {
    let text = combined_text(candidate);
    let origin = named_origin(candidate);
    let identity = has_identity_continuity(candidate);
    let in_place = has_in_place_migration_wording(&text);
    let external_port = has_external_port_wording(&text);
    let competitor = has_competitor_framing(&text);
    let real_lang = real_displaced_language(analysis);
    let strong_history = history.strong_transition
        && history
            .from_language
            .as_deref()
            .map(crate::detect::commits::is_real_application_language)
            .unwrap_or(false);

    // 1. API shims.
    if mentions_rust_api(&text) {
        return ProjectKind::Neither;
    }
    if let Some(o) = &origin {
        if target_is_rust_api(o) {
            return ProjectKind::Neither;
        }
    }

    // 2. Competitor / third-party port without same-product identity → Replacement.
    if (competitor || external_port) && !identity {
        if origin.is_some() || real_lang || competitor {
            return ProjectKind::Replacement;
        }
    }

    // 3. Rewrite: same shipping product + commit-proven cross-language migration.
    if strong_history && identity {
        return ProjectKind::Rewrite;
    }

    // Rising Rust history without product identity → third-party port / clone.
    if strong_history && (in_place || external_port || competitor) && !identity {
        return ProjectKind::Replacement;
    }

    // Same-product migration wording but commits show no real language shift
    // (born-in-Rust reimplementation, README exaggeration, keyword discovery).
    if identity && !strong_history && (in_place || external_port) {
        return ProjectKind::Replacement;
    }

    // 4. Remaining competitor / port with a target.
    if (external_port || competitor) && (origin.is_some() || real_lang || competitor) {
        return ProjectKind::Replacement;
    }

    // In-place wording without identity → not a rewrite of a shipping product.
    if in_place && (real_lang || has_targeted_migration(&text)) && !identity {
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

    // --- Rewrite: identity + evidence ------------------------------------

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
        // "coreutils" overlaps origin "gnu coreutils"
        assert!(has_identity_continuity(&c));
        assert_eq!(
            classify(&c, &a, &rising_rust_history("C")),
            ProjectKind::Rewrite
        );
    }

    // --- Replacement: third-party / competitor ---------------------------

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
        assert!(
            !has_identity_continuity(&c),
            "face ⊂ rvface must not count as identity"
        );
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

    // --- Neither ---------------------------------------------------------

    #[test]
    fn wplusplus_toy_without_identity_is_neither_or_replacement() {
        // Phrase bait + history but no shipping-product identity.
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
        // Makefile is not a real application language → not Rewrite.
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
            vec![repo_signal(
                "repo describes a rewrite of a named project",
            )],
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
    fn issue_on_product_repo_without_history_is_replacement() {
        let mut c = named("oven-sh/bun", "Incredibly fast JavaScript runtime");
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about rewriting in Rust: Rewrite Bun in Rust".into(),
            url: "https://github.com/oven-sh/bun/pull/1".into(),
        });
        let a = analysis(70.0, Some("Zig"), true);
        assert!(has_identity_continuity(&c));
        assert_eq!(classify(&c, &a, &no_history()), ProjectKind::Replacement);
    }

    #[test]
    fn issue_on_product_repo_with_history_is_rewrite() {
        let mut c = named("oven-sh/bun", "Incredibly fast JavaScript runtime");
        c.signals.push(Signal {
            kind: "pull-request".into(),
            detail: "PR titled about rewriting in Rust: Rewrite Bun in Rust".into(),
            url: "https://github.com/oven-sh/bun/pull/1".into(),
        });
        let a = analysis(70.0, Some("Zig"), true);
        assert!(has_identity_continuity(&c));
        assert_eq!(
            classify(&c, &a, &rising_rust_history("Zig")),
            ProjectKind::Rewrite
        );
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
        assert!(
            !has_identity_continuity(&c),
            "RIIR meme / wishlist must not grant identity"
        );
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

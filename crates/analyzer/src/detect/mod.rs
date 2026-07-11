//! Detection pipeline: discovery heuristics, language analysis, and scoring.

pub mod classify;
pub mod commits;
pub mod enrich;
pub mod git_history;
pub mod heuristics;
pub mod score;
pub mod transitions;

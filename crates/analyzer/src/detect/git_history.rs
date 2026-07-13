//! Shared git clone + `git log --numstat` parsing for commit-history analysis.
//!
//! Shallow depth clones are used first — they are reliable for `git log --numstat`.
//! Blob-less partial clones are faster but break numstat on many hosts (missing
//! tree objects / promisor errors), so they are only attempted as a last resort.
//!
//! Every git invocation is bounded by a wall-clock timeout. Timed-out children
//! are killed (`kill_on_drop` + explicit `kill`) so orphaned clones cannot wedge
//! a long backfill run.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::time::timeout;
use tracing::warn;

/// One file change line from `git log --numstat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub added: u64,
    pub removed: u64,
    pub path: String,
}

/// Aggregated stats for a single commit.
#[derive(Debug, Clone)]
pub struct CommitRecord {
    pub timestamp: i64,
    pub subject: String,
    /// Raw `Co-authored-by` trailer values (`Name <email>`), if any.
    pub coauthors: Vec<String>,
    pub files: Vec<FileChange>,
}

/// Clone strategies for typical repos.
const CLONE_STRATEGIES: &[&[&str]] = &[
    &["--depth", "8000", "--single-branch", "--quiet"],
    &["--depth", "3000", "--single-branch", "--quiet"],
    &["--shallow-since=5 years ago", "--single-branch", "--quiet"],
    &["--filter=blob:none", "--single-branch", "--quiet"],
];

/// Deeper strategies for exemplar / macro-commit analysis on large monorepos.
const MACRO_CLONE_STRATEGIES: &[&[&str]] = &[
    &["--depth", "50000", "--single-branch", "--quiet"],
    &["--depth", "25000", "--single-branch", "--quiet"],
    &["--depth", "12000", "--single-branch", "--quiet"],
    &["--shallow-since=10 years ago", "--single-branch", "--quiet"],
    &["--shallow-since=5 years ago", "--single-branch", "--quiet"],
    &["--filter=blob:none", "--single-branch", "--quiet"],
];

/// Clone `repo_url` and return chronological numstat log output.
///
/// `per_step` caps each individual clone/log invocation. `repo_budget` is a
/// hard wall-clock limit across all strategies so one stubborn repo cannot
/// burn unbounded wall time. Pass `macro_mode` for deeper clone strategies
/// suited to large exemplar monorepos (bun, deno, react, …).
pub async fn fetch_log(
    repo_url: &str,
    dir: &Path,
    per_step: Duration,
    repo_budget: Duration,
    macro_mode: bool,
) -> Option<String> {
    let started = Instant::now();
    let strategies: &[&[&str]] = if macro_mode {
        MACRO_CLONE_STRATEGIES
    } else {
        CLONE_STRATEGIES
    };
    for extra in strategies {
        let remaining = repo_budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            warn!(
                repo = repo_url,
                budget_secs = repo_budget.as_secs(),
                "git_history: repo budget exhausted"
            );
            break;
        }
        let step = per_step.min(remaining);
        if try_clone(repo_url, dir, extra, step).await {
            let remaining = repo_budget.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                warn!(repo = repo_url, "git_history: repo budget exhausted after clone");
                break;
            }
            let step = per_step.min(remaining);
            match log_numstat_chronological(dir, step).await {
                Some(log) if log_contains_commits(&log) => return Some(log),
                Some(_) => warn!(repo = repo_url, "git_history: empty log after clone"),
                None => {}
            }
        }
        let _ = tokio::fs::remove_dir_all(dir).await;
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    warn!(repo = repo_url, "git_history: all clone strategies failed");
    None
}

async fn try_clone(repo_url: &str, dir: &Path, extra: &[&str], limit: Duration) -> bool {
    let dir_str = dir.to_string_lossy();
    let mut args = vec!["clone"];
    args.extend_from_slice(extra);
    args.push(repo_url);
    args.push(&dir_str);

    match run_git(&args, None, limit).await {
        GitRun::Ok(out) if out.status.success() => true,
        GitRun::Ok(out) => {
            warn!(
                repo = repo_url,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "git_history: clone failed"
            );
            false
        }
        GitRun::Spawn(e) => {
            warn!(repo = repo_url, error = %e, "git_history: git not available");
            false
        }
        GitRun::TimedOut => {
            warn!(repo = repo_url, "git_history: clone timed out (child killed)");
            false
        }
    }
}

fn log_contains_commits(raw: &str) -> bool {
    raw.lines().any(|l| l.contains('\0'))
}

async fn log_numstat_chronological(dir: &Path, limit: Duration) -> Option<String> {
    // Trailers are emitted on the same logical record as the subject so the
    // numstat parser stays line-oriented (bodies with newlines would break it).
    let args = [
        "log",
        "--reverse",
        "--numstat",
        "--format=%H%x00%ct%x00%s%x00%(trailers:key=Co-authored-by,valueonly,separator=%x01)",
    ];
    match run_git(&args, Some(dir), limit).await {
        GitRun::Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        GitRun::Ok(out) => {
            warn!(
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "git_history: git log failed"
            );
            None
        }
        GitRun::Spawn(e) => {
            warn!(error = %e, "git_history: git log spawn failed");
            None
        }
        GitRun::TimedOut => {
            warn!("git_history: git log timed out (child killed)");
            None
        }
    }
}

enum GitRun {
    Ok(std::process::Output),
    Spawn(std::io::Error),
    TimedOut,
}

/// Spawn `git` with piped stdio, kill the child on timeout, and return output.
async fn run_git(args: &[&str], cwd: Option<&Path>, limit: Duration) -> GitRun {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    // Own process group on Unix so `kill(-pgid)` can reap git helpers too.
    #[cfg(unix)]
    {
        // SAFETY: runs in the child after fork, before exec; only calls setpgid.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    // Non-fatal: fall back to killing the direct child only.
                }
                Ok(())
            });
        }
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return GitRun::Spawn(e),
    };
    let pid = child.id();

    match timeout(limit, child.wait_with_output()).await {
        Ok(Ok(out)) => GitRun::Ok(out),
        Ok(Err(e)) => GitRun::Spawn(e),
        Err(_) => {
            // `wait_with_output` took ownership; dropping the cancelled future
            // runs `kill_on_drop`. Also SIGKILL the process group (helpers).
            #[cfg(unix)]
            if let Some(pid) = pid {
                unsafe {
                    let _ = libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            GitRun::TimedOut
        }
    }
}

/// Parse NUL-delimited `git log --numstat` output into commit records.
pub fn parse_numstat_log(raw: &str) -> Vec<CommitRecord> {
    let mut commits = Vec::new();
    let mut current: Option<CommitRecord> = None;

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some((_hash, rest)) = line.split_once('\0') {
            let parts: Vec<&str> = rest.splitn(3, '\0').collect();
            if parts.len() >= 2 {
                if let Ok(ts) = parts[0].parse::<i64>() {
                    if let Some(c) = current.take() {
                        commits.push(c);
                    }
                    let coauthors = parts
                        .get(2)
                        .map(|raw| {
                            raw.split('\u{1}')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    current = Some(CommitRecord {
                        timestamp: ts,
                        subject: parts[1].to_string(),
                        coauthors,
                        files: Vec::new(),
                    });
                    continue;
                }
            }
        }
        if let Some(ref mut c) = current {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 3 {
                // Binary files show "-" in added/removed columns.
                let added = cols[0].parse::<u64>().unwrap_or(0);
                let removed = cols[1].parse::<u64>().unwrap_or(0);
                c.files.push(FileChange {
                    added,
                    removed,
                    path: cols[2].to_string(),
                });
            }
        }
    }
    if let Some(c) = current {
        commits.push(c);
    }
    commits
}

/// Paths we ignore when inferring language composition (docs, lockfiles, assets).
pub fn is_ignored_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let ignored_suffixes = [
        ".md", ".txt", ".json", ".yaml", ".yml", ".toml", ".lock", ".svg", ".png",
        ".jpg", ".jpeg", ".gif", ".ico", ".woff", ".woff2", ".ttf", ".eot", ".pdf",
        ".min.js", ".map", ".snap", ".sum", ".mod",
    ];
    if ignored_suffixes.iter().any(|s| lower.ends_with(s)) {
        return true;
    }
    lower.contains("/vendor/")
        || lower.contains("/node_modules/")
        || lower.ends_with("cargo.lock")
        || lower.ends_with("package-lock.json")
        || lower.ends_with("go.sum")
}

/// Map a file path to a GitHub-style language label via extension.
pub fn language_from_path(path: &str) -> Option<&'static str> {
    if is_ignored_path(path) {
        return None;
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit('.').next().unwrap_or("");
    if ext == file {
        // Extensionless sources we still care about.
        return match file {
            "Makefile" | "GNUmakefile" => Some("Makefile"),
            "Dockerfile" => Some("Dockerfile"),
            _ => None,
        };
    }
    Some(match ext {
        "rs" => "Rust",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "py" | "pyi" | "pyx" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        "scala" | "sc" => "Scala",
        "hs" => "Haskell",
        "lua" => "Lua",
        "zig" => "Zig",
        "nim" => "Nim",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "clj" | "cljs" => "Clojure",
        "ml" | "mli" => "OCaml",
        "fs" | "fsi" | "fsx" => "F#",
        "r" => "R",
        "sh" | "bash" | "zsh" => "Shell",
        "pl" | "pm" => "Perl",
        "dart" => "Dart",
        "v" | "sv" => "Verilog",
        "asm" | "s" => "Assembly",
        "m" | "mm" => "Objective-C",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numstat_with_binary_dash() {
        let raw = "\
abc\x001700000000\x00initial\x00
100\t0\tmain.rs
def\x001700100000\x00add logo\x00Cursor Agent <cursoragent@cursor.com>\x01Claude <noreply@anthropic.com>
-\t-\tlogo.png
10\t5\tlib.py
";
        let commits = parse_numstat_log(raw);
        assert_eq!(commits.len(), 2);
        assert!(commits[0].coauthors.is_empty());
        assert_eq!(commits[0].files.len(), 1);
        assert_eq!(commits[1].files.len(), 2);
        assert_eq!(commits[1].files[1].path, "lib.py");
        assert_eq!(
            commits[1].coauthors,
            vec![
                "Cursor Agent <cursoragent@cursor.com>".to_string(),
                "Claude <noreply@anthropic.com>".to_string(),
            ]
        );
    }

    #[test]
    fn maps_extensions_and_skips_lockfiles() {
        assert_eq!(language_from_path("src/main.rs"), Some("Rust"));
        assert_eq!(language_from_path("foo.c"), Some("C"));
        assert_eq!(language_from_path("Cargo.lock"), None);
        assert!(is_ignored_path("README.md"));
    }
}

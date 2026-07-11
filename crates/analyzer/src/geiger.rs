//! Optional unsafe-Rust measurement via [`cargo-geiger`].
//!
//! ReRust is meant to run cheaply, and cloning + building every external repo
//! is anything but. So unsafe measurement is strictly opt-in (the `scan
//! --measure-unsafe` flag). When enabled, and only for repos that are primarily
//! Rust, we:
//!
//!   1. **Shallow-clone** the repo into a throwaway temp dir (`git clone
//!      --depth 1`) to avoid pulling full history.
//!   2. Run **`cargo geiger --output-format Json`** in that dir, which counts
//!      safe vs. unsafe items across the crate (and its build dependencies).
//!   3. **Parse** the JSON to compute `unsafe_percentage = unsafe_functions /
//!      total_functions * 100`.
//!   4. **Clean up** the temp dir (handled automatically by [`tempfile`]).
//!
//! Every failure mode is non-fatal: if `cargo geiger` isn't installed, the
//! clone/build fails, the repo isn't a cargo crate, or a per-repo timeout is
//! hit, we log a warning and return `None` so the scan keeps going.
//!
//! Install the tool with `cargo install cargo-geiger`.

use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

/// Default per-repo budget for clone + geiger so one slow crate can't wedge the
/// whole scan. Applied independently to the clone and the geiger run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Whether `cargo geiger` is callable. Checked once before a measuring scan so
/// we can warn early and skip the per-repo work entirely when it's missing.
pub async fn is_available() -> bool {
    match Command::new("cargo").args(["geiger", "--version"]).output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Measure the unsafe-Rust share of `repo_url`, returning a percentage in
/// `0.0..=100.0`, or `None` on any failure (all non-fatal).
///
/// `per_step_timeout` bounds both the shallow clone and the geiger invocation.
pub async fn measure_unsafe(repo_url: &str, per_step_timeout: Duration) -> Option<f64> {
    // RAII temp dir: dropped (and deleted) when this function returns.
    let tmp = match tempfile::Builder::new().prefix("rerust-geiger-").tempdir() {
        Ok(t) => t,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "geiger: could not create temp dir");
            return None;
        }
    };
    let dir = tmp.path();

    // 1. Shallow clone (no history, no blobs we don't need).
    let mut clone = Command::new("git");
    clone
        .args([
            "clone",
            "--depth",
            "1",
            "--recurse-submodules",
            "--shallow-submodules",
            "--quiet",
            repo_url,
            &dir.to_string_lossy(),
        ])
        .kill_on_drop(true);
    let child = match clone.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "geiger: git not available");
            return None;
        }
    };
    let clone_pid = child.id();
    match timeout(per_step_timeout, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => {
            warn!(
                repo = repo_url,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "geiger: shallow clone failed"
            );
            return None;
        }
        Ok(Err(e)) => {
            warn!(repo = repo_url, error = %e, "geiger: git not available");
            return None;
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = clone_pid {
                unsafe {
                    let _ = libc::kill(pid as i32, libc::SIGKILL);
                }
            }
            warn!(repo = repo_url, "geiger: clone timed out (child killed)");
            return None;
        }
    }

    // 2. Run cargo-geiger, preferring machine-readable JSON. `--quiet` keeps the
    //    progress spinner out of stdout so the JSON stays clean.
    let mut geiger = Command::new("cargo");
    geiger
        .current_dir(dir)
        .args(["geiger", "--output-format", "Json", "--quiet"])
        .kill_on_drop(true);
    let child = match geiger.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "geiger: failed to launch cargo geiger");
            return None;
        }
    };
    let geiger_pid = child.id();
    let output = match timeout(per_step_timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            warn!(repo = repo_url, error = %e, "geiger: failed to launch cargo geiger");
            return None;
        }
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = geiger_pid {
                unsafe {
                    let _ = libc::kill(pid as i32, libc::SIGKILL);
                }
            }
            warn!(repo = repo_url, "geiger: measurement timed out (child killed)");
            return None;
        }
    };

    // cargo-geiger can exit non-zero even after producing usable JSON (e.g. it
    // flags unsafe usage), so try to parse stdout regardless of exit status.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(pct) = parse_geiger_json(&stdout) {
        debug!(repo = repo_url, unsafe_percentage = pct, "geiger: measured (json)");
        return Some(pct);
    }
    if let Some(pct) = parse_geiger_text(&stdout) {
        debug!(repo = repo_url, unsafe_percentage = pct, "geiger: measured (text)");
        return Some(pct);
    }

    warn!(
        repo = repo_url,
        status = %output.status,
        "geiger: produced no parseable metrics"
    );
    None
}

/// Compute the unsafe-function percentage from cargo-geiger's JSON report.
///
/// The report schema (`cargo-geiger-serde`) nests, per package, an `unsafety`
/// block with `used`/`unused` `CounterBlock`s, each of which carries a
/// `functions` `{ "safe": u64, "unsafe_": u64 }` counter. Rather than hard-code
/// the (version-sensitive) container shape, we walk the JSON tree and sum every
/// `functions` counter we find, which is robust across geiger releases and the
/// map-vs-array way `packages` may serialize.
fn parse_geiger_json(stdout: &str) -> Option<f64> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let mut safe = 0u64;
    let mut unsafe_ = 0u64;
    accumulate_functions(&value, &mut safe, &mut unsafe_);
    ratio(unsafe_, safe + unsafe_)
}

/// Recursively sum `functions` safe/unsafe counters throughout the report.
fn accumulate_functions(value: &Value, safe: &mut u64, unsafe_: &mut u64) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(functions)) = map.get("functions") {
                let s = functions.get("safe").and_then(Value::as_u64);
                // cargo-geiger names the field `unsafe_`; accept `unsafe` too.
                let u = functions
                    .get("unsafe_")
                    .or_else(|| functions.get("unsafe"))
                    .and_then(Value::as_u64);
                if let (Some(s), Some(u)) = (s, u) {
                    *safe += s;
                    *unsafe_ += u;
                }
            }
            for v in map.values() {
                accumulate_functions(v, safe, unsafe_);
            }
        }
        Value::Array(items) => {
            for v in items {
                accumulate_functions(v, safe, unsafe_);
            }
        }
        _ => {}
    }
}

/// Best-effort fallback for the ascii-tree output: the first `used/total`
/// metric column on each crate row is the function counter. We sum the totals
/// (the `y` in geiger's `x/y`) as an approximation of overall unsafe density.
fn parse_geiger_text(stdout: &str) -> Option<f64> {
    let mut unsafe_total = 0u64;
    let mut all_total = 0u64;
    for line in stdout.lines() {
        // Rows look like: "1/6  0/0  ...  ☢️  some-crate 1.2.3". Grab the first
        // "x/y" token and treat y as functions found, x as unsafe used.
        if let Some(token) = line.split_whitespace().find(|t| is_metric(t)) {
            if let Some((x, y)) = token.split_once('/') {
                if let (Ok(x), Ok(y)) = (x.parse::<u64>(), y.parse::<u64>()) {
                    unsafe_total += x;
                    all_total += y;
                }
            }
        }
    }
    ratio(unsafe_total, all_total)
}

/// True for tokens shaped like `"<digits>/<digits>"`.
fn is_metric(token: &str) -> bool {
    match token.split_once('/') {
        Some((x, y)) => {
            !x.is_empty()
                && !y.is_empty()
                && x.bytes().all(|b| b.is_ascii_digit())
                && y.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// `numerator / denominator * 100`, or `None` when there's nothing to measure.
fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        return None;
    }
    let pct = (numerator as f64 / denominator as f64) * 100.0;
    Some((pct * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_json_report() {
        let json = r#"{
            "packages": [
              [
                {"id": "root 0.1.0"},
                {"unsafety": {
                    "used":   {"functions": {"safe": 90, "unsafe_": 10}, "exprs": {"safe": 1, "unsafe_": 0}},
                    "unused": {"functions": {"safe": 0,  "unsafe_": 0}},
                    "forbids_unsafe": false
                }}
              ]
            ]
        }"#;
        // 10 unsafe / 100 total functions => 10%.
        assert_eq!(parse_geiger_json(json), Some(10.0));
    }

    #[test]
    fn json_with_no_functions_is_none() {
        assert_eq!(parse_geiger_json(r#"{"packages": []}"#), None);
    }

    #[test]
    fn parses_ascii_tree_totals() {
        let text = "\
Functions  Expressions  Impls  Traits  Methods  Dependency
2/8        0/0          0/0    0/0     0/0      ☢️  root 0.1.0
1/2        0/0          0/0    0/0     0/0      ☢️  dep 1.0.0";
        // (2 + 1) unsafe / (8 + 2) total => 30%.
        assert_eq!(parse_geiger_text(text), Some(30.0));
    }
}

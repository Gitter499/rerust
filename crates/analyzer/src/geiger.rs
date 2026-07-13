//! Optional unsafe-Rust measurement via `cargo-geiger` (`scan --measure-unsafe`).
//!
//! Shallow-clones the repo, runs `cargo geiger --output-format Json`, and returns
//! `unsafe_functions / total_functions * 100`. Any failure yields `None`.

use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn is_available() -> bool {
    match Command::new("cargo").args(["geiger", "--version"]).output().await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Measure unsafe share of `repo_url` (`0.0..=100.0`), or `None` on failure.
pub async fn measure_unsafe(repo_url: &str, per_step_timeout: Duration) -> Option<f64> {
    let tmp = match tempfile::Builder::new().prefix("rerust-geiger-").tempdir() {
        Ok(t) => t,
        Err(e) => {
            warn!(repo = repo_url, error = %e, "geiger: could not create temp dir");
            return None;
        }
    };
    let dir = tmp.path();

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

    // geiger can exit non-zero after producing usable JSON; parse anyway.
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

fn parse_geiger_json(stdout: &str) -> Option<f64> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let mut safe = 0u64;
    let mut unsafe_ = 0u64;
    accumulate_functions(&value, &mut safe, &mut unsafe_);
    ratio(unsafe_, safe + unsafe_)
}

fn accumulate_functions(value: &Value, safe: &mut u64, unsafe_: &mut u64) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(functions)) = map.get("functions") {
                let s = functions.get("safe").and_then(Value::as_u64);
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

fn parse_geiger_text(stdout: &str) -> Option<f64> {
    let mut unsafe_total = 0u64;
    let mut all_total = 0u64;
    for line in stdout.lines() {
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
        assert_eq!(parse_geiger_text(text), Some(30.0));
    }
}

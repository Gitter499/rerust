//! Static site generation.
//!
//! Renders a self-contained `index.html` (dark card grid + vanilla-JS
//! sort/filter/search) plus a `data.json` for external consumers. The project
//! data is embedded directly into the HTML so the page works when opened from
//! disk as well as when served from GitHub Pages.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use minijinja::{context, Environment};

use crate::types::Project;

const TEMPLATE: &str = include_str!("templates/index.html.j2");

/// Write `index.html` and `data.json` into `out_dir` for the given projects.
pub fn build(out_dir: &str, projects: &[Project]) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("create {out_dir}"))?;

    let generated_at = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let data_json = serde_json::to_string_pretty(projects)?;
    fs::write(Path::new(out_dir).join("data.json"), &data_json).context("write data.json")?;

    let embedded = serde_json::to_string(projects)?.replace('<', "\\u003c");
    let mut env = Environment::new();
    env.add_template("index", TEMPLATE)
        .context("load index template")?;
    let tmpl = env.get_template("index")?;
    let html = tmpl
        .render(context! {
            generated_at => generated_at,
            project_count => projects.len(),
            data_json => embedded,
        })
        .context("render index template")?;

    fs::write(Path::new(out_dir).join("index.html"), html).context("write index.html")?;
    Ok(())
}

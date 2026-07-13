//! Static site generation: self-contained `index.html` + `data.json`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use minijinja::{context, Environment};

use crate::types::Project;

const TEMPLATE: &str = include_str!("templates/index.html.j2");
const CUSTOM_DOMAIN: &str = "reru.st\n";

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
    // Always publish the Pages custom-domain file with the artifact (workflow
    // deploys replace the published tree; relying on a leftover docs/CNAME is fragile).
    fs::write(Path::new(out_dir).join("CNAME"), CUSTOM_DOMAIN).context("write CNAME")?;

    let assets_src = Path::new("docs/assets");
    let assets_dst = Path::new(out_dir).join("assets");
    if assets_src.is_dir() && assets_src != assets_dst {
        copy_dir_all(assets_src, &assets_dst)
            .with_context(|| format!("copy assets {} → {}", assets_src.display(), assets_dst.display()))?;
    }

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

# ReRust

[![Pages](https://img.shields.io/badge/site-reru.st-blue)](https://reru.st/)
[![scan-and-deploy](https://github.com/Gitter499/rerust/actions/workflows/scan.yml/badge.svg)](https://github.com/Gitter499/rerust/actions/workflows/scan.yml)
[![enrich-and-deploy](https://github.com/Gitter499/rerust/actions/workflows/enrich.yml/badge.svg)](https://github.com/Gitter499/rerust/actions/workflows/enrich.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

![ReRust](docs/assets/logo-lockup-144.png)

Find open-source projects migrating to Rust. Classify each hit as rewrite, replacement, or noise. Publish results to [reru.st](https://reru.st/).

## Build and preview

```bash
cargo build --release
export GITHUB_TOKEN="$(gh auth token)"
./target/release/rerust scan --no-analyze-history
./target/release/rerust build-site --out docs
python3 -m http.server -d docs 8000
```

## Production

Scans and enrichment run on GitHub Actions: [Gitter499/rerust](https://github.com/Gitter499/rerust). Site deploys to [reru.st](https://reru.st/).

| Workflow | Schedule |
|----------|----------|
| `scan.yml` | Daily 06:00 UTC |
| `enrich.yml` | Weekly Sun 08:00 UTC |

Open the Actions tab and dispatch either workflow.

`scripts/backfill-watch.sh` is dev-only. Do not use it for production data.

## License

MIT

# ReRust

![ReRust](docs/assets/logo-lockup-144.png)

Find open-source projects migrating to Rust. Classify each hit as rewrite, replacement, or noise. Publish results to `docs/`.

## Build and preview

```bash
cargo build --release
export GITHUB_TOKEN="$(gh auth token)"
./target/release/rerust scan --no-analyze-history
./target/release/rerust build-site --out docs
python3 -m http.server -d docs 8000
```

## Production

Scans and enrichment run on GitHub Actions: [Gitter499/rerust](https://github.com/Gitter499/rerust).

| Workflow | Schedule |
|----------|----------|
| `scan.yml` | Daily 06:00 UTC |
| `enrich.yml` | Weekly Sun 08:00 UTC |

Open the Actions tab and dispatch either workflow.

`scripts/backfill-watch.sh` is dev-only. Do not use it for production data.

## License

MIT

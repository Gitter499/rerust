#!/bin/bash
# Serve the generated ReRust site on the HOST machine (not the agent worker).
# Glass / Simple Browser can only reach servers started in your own terminal.
set -euo pipefail
cd "$(dirname "$0")/../docs"
PORT="${1:-8000}"
echo "Serving ReRust docs at http://127.0.0.1:${PORT}/"
echo "Press Ctrl+C to stop."
exec python3 -m http.server "$PORT" --bind 127.0.0.1

#!/usr/bin/env bash
# Build, free :3000 if needed, start the spread app from repo root (loads .env).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
pid="$(lsof -i :3000 -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
if [[ -n "${pid}" ]]; then
  kill "${pid}" && sleep 1
fi
echo "→ http://127.0.0.1:3000"
echo "→ POST /api/reset-realized-trades — clears open/closed sim + route stats + data/closed-trades.jsonl"
echo "→ python3 scripts/analyze_closed_trades.py"
exec cargo run --release --quiet

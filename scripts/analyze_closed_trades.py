#!/usr/bin/env python3
"""
Analyze simulated closed trades (JSON lines from data/closed-trades.jsonl
or a JSON array export from GET /api/tickers -> closedTrades).

Usage:
  python3 scripts/analyze_closed_trades.py
  python3 scripts/analyze_closed_trades.py data/closed-trades.jsonl
  python3 scripts/analyze_closed_trades.py --target-win 0.75 --min-trades 15

Does not prove future 75% win rate; only summarizes history and
ex-post filters (e.g. minimum entryEstimatedNet) that would have changed
which past trades counted — useful for tuning thresholds once N is large.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def load_trades(path: Path | None) -> list[dict]:
    rows: list[dict] = []
    if path is None:
        path = Path(__file__).resolve().parent.parent / "data" / "closed-trades.jsonl"
    if not path.exists():
        return rows
    if path.suffix.lower() == ".json":
        data = json.loads(path.read_text())
        return list(data) if isinstance(data, list) else []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        rows.append(json.loads(line))
    return rows


def win_rate(trades: list[dict]) -> float | None:
    if not trades:
        return None
    wins = sum(1 for t in trades if float(t.get("realizedNet", 0.0)) > 0.0)
    return wins / len(trades)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "path",
        nargs="?",
        type=Path,
        default=None,
        help="closed-trades.jsonl or a JSON file containing a closedTrades array",
    )
    ap.add_argument(
        "--target-win",
        type=float,
        default=0.75,
        help="target win rate (realizedNet > 0) for threshold search",
    )
    ap.add_argument(
        "--min-trades",
        type=int,
        default=20,
        help="minimum trades required after a filter to report a threshold",
    )
    args = ap.parse_args()

    default_log = Path(__file__).resolve().parent.parent / "data" / "closed-trades.jsonl"
    trades = load_trades(args.path)
    n = len(trades)
    log_path = args.path if args.path is not None else default_log
    print(f"trades loaded: {n} (from {log_path.resolve()})")
    if n == 0:
        print(
            "No closes logged yet. The bot appends one line per simulated position close.\n"
            "If `opened` in /api/tickers stays 0, entry gates are still blocking — rebuild/restart\n"
            "the server after strategy changes, then wait past basis warmup (~54s at 1500ms poll)."
        )
        return 1

    wr = win_rate(trades)
    assert wr is not None
    print(f"overall win rate (realizedNet > 0): {wr:.1%}")
    nets = [float(t["realizedNet"]) for t in trades]
    print(f"mean realizedNet: {sum(nets)/n:.4f}  min: {min(nets):.4f}  max: {max(nets):.4f}")

    reasons = Counter(str(t.get("closedReason", "?")) for t in trades)
    print("closedReason counts:", dict(reasons))

    routes = Counter(f"{t.get('buyExchange')}->{t.get('sellExchange')}" for t in trades)
    print("top routes:")
    for r, c in routes.most_common(8):
        sub = [t for t in trades if f"{t.get('buyExchange')}->{t.get('sellExchange')}" == r]
        wr_r = win_rate(sub)
        print(f"  {c:4d}  {wr_r:.1%}  {r}")

    # Ex-post: require larger modeled edge at entry -> often higher win rate, fewer trades.
    entries = sorted({float(t.get("entryEstimatedNet", 0.0)) for t in trades})
    target = args.target_win
    min_n = args.min_trades
    best_t: float | None = None
    best_sub: list[dict] = []
    for t in entries:
        sub = [x for x in trades if float(x.get("entryEstimatedNet", 0.0)) >= t]
        if len(sub) < min_n:
            continue
        wr_t = win_rate(sub)
        if wr_t is not None and wr_t >= target:
            best_t = t
            best_sub = sub

    print()
    if best_t is None:
        print(
            f"No entryEstimatedNet floor (with >= {min_n} trades) reached {target:.0%} win rate.\n"
            "Collect more closes, lower --min-trades, or accept a lower target."
        )
    else:
        wr_b = win_rate(best_sub)
        print(
            f"Ex-post filter: entryEstimatedNet >= {best_t:.4f}  ->  n={len(best_sub)}  win={wr_b:.1%}\n"
            "Interpretation: past trades above that modeled edge tended to win more often.\n"
            "To align the bot, raise minDepthNetThreshold / entry gates — not identical, but correlated."
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

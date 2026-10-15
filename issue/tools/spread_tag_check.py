#!/usr/bin/env python3
"""Spread tag (spec spread-position-tag) of every closed pair, next to its price (spread) PnL.
Read-only on funding.db. Presents numbers only; it draws no causal conclusion.

Usage:
    python3 issue/tools/spread_tag_check.py [KEYWORD ...]
    TONG_FUNDING_DB=/path/to/funding.db python3 issue/tools/spread_tag_check.py

Tag definition (same as core/src/spread_tag.rs): long-leg-exchange price < short-leg-exchange price
= positive, > = negative, equal = neutral; pct = (short - long) / long * 100.
Three observation points per pair:
  T-10 snapshot   entry_snapshot (PAIR_TRANSITION to=ORDER_SUBMIT): long/short `mark_price`
                  (falls back to `expected_price` when mark_price is absent)
  entry fills     ORDER_FILL action=open  avg_price (long leg vs short leg)
  exit fills      ORDER_FILL action=close avg_price
Price PnL (real) = long (close - open) * qty + short (open - close) * qty, as in trade_table.py.
"""
import json
import os
import sqlite3
import sys
from decimal import Decimal as D

DB = os.environ.get("TONG_FUNDING_DB") or os.path.expanduser("~/Library/Application Support/tong-funding/funding.db")
MINUS = "−"


def tag(long_px, short_px):
    long_px, short_px = D(long_px), D(short_px)
    if long_px == 0:
        return "unknown", None
    pct = (short_px - long_px) / long_px * 100
    return ("positive" if long_px < short_px else "negative" if long_px > short_px else "neutral"), pct


def sg(x, n=4):
    x = D(x)
    return f"+{x:.{n}f}" if x > 0 else (f"{MINUS}{abs(x):.{n}f}" if x < 0 else f"{x:.{n}f}")


def cell(long_px, short_px):
    k, p = tag(long_px, short_px)
    return f"{k} {sg(p, 4)}%" if p is not None else k


def row(db, pid):
    ev = [(ts, e, json.loads(p)) for ts, e, p in db.execute("select ts_ms,event_type,payload from events where pair_id=? order by id", (pid,))]
    snap = next(p["detail"]["entry_snapshot"] for _, e, p in ev if e == "PAIR_TRANSITION" and p.get("to") == "ORDER_SUBMIT")
    fills = {(p["leg"], p["action"]): p for _, e, p in ev if e == "ORDER_FILL"}
    px = lambda s: s.get("mark_price") or s["expected_price"]
    lo, so = D(fills[("long", "open")]["avg_price"]), D(fills[("short", "open")]["avg_price"])
    lc, sc = D(fills[("long", "close")]["avg_price"]), D(fills[("short", "close")]["avg_price"])
    qty_l, qty_s = D(fills[("long", "open")]["filled_quantity"]), D(fills[("short", "open")]["filled_quantity"])
    pnl = (lc - lo) * qty_l + (so - sc) * qty_s
    return dict(
        pid=pid, symbol=pid.split("-")[-1].upper(), long=snap["long"]["exchange"], short=snap["short"]["exchange"],
        t10=cell(px(snap["long"]), px(snap["short"])), entry=cell(lo, so), exit=cell(lc, sc), pnl=pnl,
        entry_pct=tag(lo, so)[1], exit_pct=tag(lc, sc)[1],
    )


def main():
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    pairs = [r[0] for r in db.execute("select distinct pair_id from events where event_type='CLOSE_CONFIRMED' order by id")]
    keys = [k.lower() for k in sys.argv[1:]]
    if keys:
        pairs = [p for p in pairs if any(k in p.lower() for k in keys)]
    if not pairs:
        sys.exit("no matching closed pair")
    print("| 配對 | 多腿 / 空腿 | T-10 快照標籤 | 進場成交價標籤 | 出場成交價標籤 | 價差百分比 出場−進場 | 價差損益（real，USDT） |")
    print("|---|---|---|---|---|---|---|")
    for pid in pairs:
        r = row(db, pid)
        move = sg(r["exit_pct"] - r["entry_pct"], 4) + "%" if r["entry_pct"] is not None and r["exit_pct"] is not None else "-"
        print(f"| `{r['pid']}` | {r['long']} / {r['short']} | {r['t10']}% | {r['entry']}% | {r['exit']}% | {move} | {sg(r['pnl'])} |".replace("unknown%", "unknown").replace("%%", "%"))


if __name__ == "__main__":
    main()

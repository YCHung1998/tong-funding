#!/usr/bin/env python3
"""Replays the require-synchronized-settlement rule over entered pairs (read-only).

For every pair that reached ORDER_SUBMIT, take the entry snapshot recorded by the pre-trade check
(PAIR_TRANSITION to ORDER_SUBMIT, detail.entry_snapshot) and apply the new rule:
    synchronized  <=>  long.next_funding_time == short.next_funding_time  and  > 0   (0 = missing)
Prints one line per pair: pair id, both next_funding_time (UTC), verdict (ENTER / BLOCKED).

Usage:
    python3 issue/tools/check_sync_rule.py [KEYWORD ...]     # e.g. rsr blur ; none = every pair
    TONG_FUNDING_DB=/path/to/funding.db python3 issue/tools/check_sync_rule.py

The database is opened with SQLite URI mode=ro; nothing is written.
"""
import datetime
import json
import os
import sqlite3
import sys

DB = os.environ.get("TONG_FUNDING_DB") or os.path.expanduser("~/Library/Application Support/tong-funding/funding.db")


def utc(ms):
    if not ms:
        return "missing"
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime("%m-%d %H:%M")


def legs_settle_together(long_ms, short_ms):
    """Same as core::net_edge::legs_settle_together."""
    return bool(long_ms) and long_ms == short_ms


def main(keywords):
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    pair_ids = [r[0] for r in db.execute("select pair_id from events where pair_id is not null group by pair_id order by min(id)")]
    rows = []
    for pid in pair_ids:
        if keywords and not any(k.lower() in pid.lower() for k in keywords):
            continue
        snap = None
        for (payload,) in db.execute("select payload from events where pair_id=? and event_type='PAIR_TRANSITION' order by id", (pid,)):
            p = json.loads(payload)
            if p.get("to") == "ORDER_SUBMIT":
                snap = (p.get("detail") or {}).get("entry_snapshot")
                break
        if not snap:
            continue
        lt, st = snap["long"].get("next_funding_time"), snap["short"].get("next_funding_time")
        ok = legs_settle_together(lt, st)
        rows.append((pid, utc(lt), utc(st), "ENTER (synchronized)" if ok else "BLOCKED (not synchronized)"))
    print(f"{'pair':44} {'long next':>12} {'short next':>12}  verdict")
    for r in rows:
        print(f"{r[0]:44} {r[1]:>12} {r[2]:>12}  {r[3]}")
    print(f"\n{len(rows)} pairs, {sum(1 for r in rows if r[3].startswith('BLOCKED'))} blocked by the new rule")


if __name__ == "__main__":
    main(sys.argv[1:])

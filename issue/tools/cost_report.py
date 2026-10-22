#!/usr/bin/env python3
"""Cost calibration report of finalized pairs, read from funding.db (read-only).

Usage:
    python3 issue/tools/cost_report.py
    TONG_FUNDING_DB=/path/to/funding.db python3 issue/tools/cost_report.py

Per FINALIZED pair the latest PAIR_PNL_COMPUTED / PAIR_PNL_RECOMPUTED payload's `attribution` is used.
Older events have none: the numbers are then restored from the raw events (entry reference = the T-10
snapshot mark price, fills = ORDER_FILL, close reference = ORDER_SUBMITTED reference_price) and the row
is marked 還原值. Formulas follow openspec/changes/cost-attribution (dir: long open -1 / close +1, short opposite):
  entry slippage = sum(open fills) dir*(ref-act)*q      (positive = unfavourable)
  exit slippage  = sum(close fills) dir*(ref-act)*q     (positive = unfavourable)
  drift          = sum(all fills) dir*ref*q             (price pnl using reference prices only)
  net            = funding + drift - entry - exit - fees
  percentages are of the notional per leg = mean of q*act over the two opening fills.
Unknown input -> unknown item (None, printed as a dash), never 0. The database is opened with mode=ro.
"""
import json
import os
import sqlite3
import sys
from decimal import Decimal as D

DB = os.environ.get("TONG_FUNDING_DB") or os.path.expanduser("~/Library/Application Support/tong-funding/funding.db")
MINUS = "−"
DASH = "—"
ITEMS = ("entry_slippage", "drift", "exit_slippage", "fees", "funding", "net")
MIN_SAMPLES = 30
HEADER = "【說明】demo 環境的數字不代表正式環境；建議值僅供參考，不會自動套用到任何設定。"


def connect(path):
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def dec(x):
    return None if x is None else D(str(x))


def unknown_item():
    return {"usdt": None, "pct": None}


def item(usdt, notional):
    if usdt is None:
        return unknown_item()
    return {"usdt": usdt, "pct": (usdt / notional * 100) if notional else None}


def from_attribution(pid, a):
    notional = dec(a.get("notional_per_leg"))
    row = {"pid": pid, "restored": False, "notional_per_leg": notional}
    for k in ITEMS:
        v = a.get(k) or {}
        row[k] = {"usdt": dec(v.get("usdt")), "pct": dec(v.get("pct"))}
    return row


def restore(pid, events, pnl):
    snap = next((p["detail"]["entry_snapshot"] for _, e, p in events
                 if e == "PAIR_TRANSITION" and p.get("to") == "ORDER_SUBMIT"), {})
    fills = {(p["leg"], p["action"]): p for _, e, p in events if e == "ORDER_FILL"}
    close_ref = {(p["leg"], p["action"]): p.get("reference_price") for _, e, p in events
                 if e == "ORDER_SUBMITTED" and p.get("action") == "close"}

    def ref(side, action):
        if action == "open":
            return dec((snap.get(side) or {}).get("mark_price"))
        r = close_ref.get((side, action))
        if r is None and (side, action) in fills:
            r = fills[(side, action)].get("reference_price")
        return dec(r)

    def px(side, action):  # (q, act) or None
        f = fills.get((side, action))
        if not f or f.get("avg_price") is None or f.get("filled_quantity") is None:
            return None
        return dec(f["filled_quantity"]), dec(f["avg_price"])

    opens = [px(s, "open") for s in ("long", "short")]
    notional = sum(q * a for q, a in opens) / 2 if all(opens) else None

    def direction(side, action):  # long: open -1, close +1; short opposite
        return (-1 if action == "open" else 1) * (1 if side == "long" else -1)

    def slip(action):
        total = D(0)
        for side in ("long", "short"):
            r, f = ref(side, action), px(side, action)
            if r is None or f is None:
                return None
            total += direction(side, action) * (r - f[1]) * f[0]
        return total

    drift = D(0)
    for side in ("long", "short"):
        for action in ("open", "close"):
            r, f = ref(side, action), px(side, action)
            if r is None or f is None:
                drift = None
                break
            drift += direction(side, action) * r * f[0]
        if drift is None:
            break

    fee_list = [dec(fills[k].get("fee")) if k in fills else None for k in (("long", "open"), ("short", "open"), ("long", "close"), ("short", "close"))]
    fees = None if any(f is None for f in fee_list) else sum(fee_list)

    funding = None
    if pnl:
        legs = (pnl.get("breakdown") or {}).get("legs") or []
        vals = [dec((l.get("components") or {}).get("funding")) for l in legs]
        if legs and all(v is not None for v in vals) and not any("Funding" in (l.get("missing") or []) for l in legs):
            funding = sum(vals)

    entry, exit_ = slip("open"), slip("close")
    parts = (funding, drift, entry, exit_, fees)
    net = None if any(p is None for p in parts) else funding + drift - entry - exit_ - fees
    row = {"pid": pid, "restored": True, "notional_per_leg": notional}
    for k, v in zip(("funding", "drift", "entry_slippage", "exit_slippage", "fees", "net"), (funding, drift, entry, exit_, fees, net)):
        row[k] = item(v, notional)
    return row


def load_pairs(db):
    pids = []
    for pid, payload in db.execute("select pair_id, payload from events where event_type='PAIR_TRANSITION' and payload like '%FINALIZED%' order by id"):
        if json.loads(payload).get("to") == "FINALIZED" and pid not in pids:
            pids.append(pid)
    rows = []
    for pid in pids:
        events = [(ts, e, json.loads(p)) for ts, e, p in db.execute(
            "select ts_ms,event_type,payload from events where pair_id=? order by id", (pid,))]
        pnls = [p for _, e, p in events if e in ("PAIR_PNL_COMPUTED", "PAIR_PNL_RECOMPUTED")]
        if not pnls:
            continue
        latest = pnls[-1]
        # latest event first; fall back to the newest one that carries the field
        att = latest.get("attribution") or next((p["attribution"] for p in reversed(pnls) if p.get("attribution")), None)
        try:
            rows.append(from_attribution(pid, att) if att else restore(pid, events, latest))
        except (KeyError, TypeError, ValueError, ArithmeticError):
            continue  # cannot be read at all: skipped, not guessed
    return rows


# ---------- formatting ----------
def sg_pct(x):
    if x is None:
        return DASH
    return f"+{x:.4f}%" if x >= 0 else f"{MINUS}{abs(x):.4f}%"


def cost_pct(x):  # a cost: positive = paid, in parentheses; negative = favourable, shown as income
    if x is None:
        return DASH
    return f"({x:.4f}%)" if x >= 0 else f"+{abs(x):.4f}%"


def percentile(values, p):
    v = sorted(values)
    if not v:
        return None
    k = (len(v) - 1) * D(p) / 100
    lo = int(k)
    hi = min(lo + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def pcts(rows, key):
    return [r[key]["pct"] for r in rows if r[key]["pct"] is not None]


def render_report(rows):
    n = len(rows)
    out = [HEADER, "", f"## 成本校準報表（樣本數 n={n}）", ""]
    if n < MIN_SAMPLES:
        out += [f"> 樣本不足，僅供參考（n={n} < {MIN_SAMPLES}）。", ""]
    out += ["成本放括號、損益帶 +/− 符號；滑價與手續費為成本（括號內為不利），漂移、資金費、淨為損益；百分比皆為每腿名目 %。「—」＝資料不足，不是 0。", "",
            "| 配對 | 來源 | 進場滑價% | 漂移% | 出場滑價% | 手續費% | 資金費% | 淨% |", "|---|---|---|---|---|---|---|---|"]
    for r in rows:
        out.append("| {} | {} | {} | {} | {} | {} | {} | {} |".format(
            r["pid"], "還原值" if r["restored"] else "attribution",
            cost_pct(r["entry_slippage"]["pct"]), sg_pct(r["drift"]["pct"]), cost_pct(r["exit_slippage"]["pct"]),
            cost_pct(r["fees"]["pct"]), sg_pct(r["funding"]["pct"]), sg_pct(r["net"]["pct"])))
    out.append("")

    # share of total cost (pct basis, over pairs where the item is known)
    adverse = [max(D(0), -x) for x in pcts(rows, "drift")]
    comps = [("進場滑價", sum(pcts(rows, "entry_slippage"), D(0))), ("出場滑價", sum(pcts(rows, "exit_slippage"), D(0))),
             ("不利漂移", sum(adverse, D(0))), ("手續費", sum(pcts(rows, "fees"), D(0)))]
    comps = [(k, max(v, D(0))) for k, v in comps]
    total = sum(v for _, v in comps)
    out += ["### 各項佔總成本比例（加總每腿名目 %；有利的滑價與漂移不計入）", "", "| 項目 | 佔比 |", "|---|---|"]
    for k, v in comps:
        out.append(f"| {k} | {(v / total * 100):.1f}% |" if total else f"| {k} | {DASH} |")
    out.append("")

    tot_slip = [r["entry_slippage"]["pct"] + r["exit_slippage"]["pct"] for r in rows
                if r["entry_slippage"]["pct"] is not None and r["exit_slippage"]["pct"] is not None]
    out += ["### 分位數（每腿名目 %）", "", "| 項目 | p50 | p75 | p90 | 樣本 |", "|---|---|---|---|---|"]
    suggestions = {}
    for label, vals in (("總滑價（進場＋出場）", tot_slip), ("不利漂移（漂移為負時的絕對值，有利記 0）", adverse)):
        qs = [percentile(vals, p) for p in (50, 75, 90)]
        suggestions[label] = qs
        out.append(f"| {label} | " + " | ".join(DASH if q is None else f"{q:.4f}%" for q in qs) + f" | {len(vals)} |")
    out.append("")

    slip_q, adv_q = suggestions["總滑價（進場＋出場）"], suggestions["不利漂移（漂移為負時的絕對值，有利記 0）"]
    out += ["### 建議值（只輸出，不會自動套用）", "", "est_slippage_pct = 總滑價分位數 ÷ 4（Net Edge 以 4 次成交計）；safety_margin_pct = 不利漂移分位數。", "",
            "| 設定 | p50 | p75 | p90 |", "|---|---|---|---|"]
    out.append("| est_slippage_pct | " + " | ".join(DASH if q is None else f"{q / 4:.4f}" for q in slip_q) + " |")
    out.append("| safety_margin_pct | " + " | ".join(DASH if q is None else f"{q:.4f}" for q in adv_q) + " |")
    out += ["", f"樣本數 n={n}" + ("；樣本不足，僅供參考。" if n < MIN_SAMPLES else "。"), ""]
    return "\n".join(out)


def main():
    db = connect(DB)
    rows = load_pairs(db)
    if not rows:
        sys.exit("no finalized pair with a PnL event")
    print(render_report(rows))


if __name__ == "__main__":
    main()

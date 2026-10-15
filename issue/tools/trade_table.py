#!/usr/bin/env python3
"""Standard trade table of one finalized pair, read from funding.db (read-only).

Usage:
    python3 issue/tools/trade_table.py [KEYWORD ...]      # e.g. blur  |  1791982970031  |  (none = every closed pair)
    TONG_FUNDING_DB=/path/to/funding.db python3 issue/tools/trade_table.py rsr

What it prints (markdown): A basic info and timeline, B per-leg stage table, C summary.
Conventions: income `+`, loss `-` (printed as the minus sign), COSTS in parentheses `( )`.
Funding rate of the T-10 snapshot: positive = the long pays, the short receives.

Self-check: the computed "actual net" must equal the net of the latest PnL event in the database,
otherwise the script stops (so a wrong definition cannot print a plausible-looking table).
The Binance demo settlement rate is read from the PUBLIC demo endpoint (no key); offline it prints a dash.
"""
import datetime
import json
import os
import sqlite3
import sys
import urllib.request
from decimal import Decimal as D

DB = os.environ.get("TONG_FUNDING_DB") or os.path.expanduser("~/Library/Application Support/tong-funding/funding.db")
MINUS = "−"


def sg(x, n=4):
    x = D(x)
    return f"+{x:.{n}f}" if x > 0 else (f"{MINUS}{abs(x):.{n}f}" if x < 0 else f"{x:.{n}f}")


def cost(x, n=4):
    return f"({abs(D(x)):.{n}f})"


def pct(x):
    x = D(x) * 100
    return f"+{x:.4f}%" if x >= 0 else f"{MINUS}{abs(x):.4f}%"


def utc(ms, fmt="%m-%d %H:%M:%S"):
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime(fmt)


def demo_rate(symbol, settle_ms):
    try:
        url = f"https://demo-fapi.binance.com/fapi/v1/fundingRate?symbol={symbol}&startTime={settle_ms - 1000}&endTime={settle_ms + 1000}"
        rows = json.load(urllib.request.urlopen(url, timeout=15))
        return pct(rows[0]["fundingRate"]) if rows else "(no settlement at this time)"
    except Exception:
        return "-"


def pair_table(db, pid):
    ev = [(ts, e, json.loads(p)) for ts, e, p in db.execute("select ts_ms,event_type,payload from events where pair_id=? order by id", (pid,))]
    prepared = next(p for _, e, p in ev if e == "PAIR_PREPARED")
    settle_ms = prepared["settlement_ms"]
    check = next((ts, p["detail"]["entry_snapshot"]) for ts, e, p in ev if e == "PAIR_TRANSITION" and p.get("to") == "ORDER_SUBMIT")
    snap = check[1]
    pnl = [p for _, e, p in ev if e in ("PAIR_PNL_COMPUTED", "PAIR_PNL_RECOMPUTED")][-1]
    legs_pnl = {leg["exchange"]: leg for leg in pnl["breakdown"]["legs"]}
    symbol = pid.split("-")[-1].upper()
    short_name = symbol.replace("USDT", "")

    fills = {(p["leg"], p["action"]): (ts, p) for ts, e, p in ev if e == "ORDER_FILL"}
    lat = {(p["leg"], p["action"]): p for _, e, p in ev if e == "ORDER_LATENCY"}
    closed_ts = next((ts for ts, e, _ in ev if e == "CLOSE_CONFIRMED"), None)
    linked = {p["exchange"]: (ts, p) for ts, e, p in ev if e == "PAIR_FUNDING_LINKED"}

    legs = {}
    for side in ("long", "short"):
        s = snap[side]
        ex = s["exchange"]
        o, c = fills[(side, "open")][1], fills[(side, "close")][1]
        qty, po, pc = D(o["filled_quantity"]), D(o["avg_price"]), D(c["avg_price"])
        rate = D(s["funding_rate"])
        settles = s["next_funding_time"] == settle_ms
        notional = qty * po
        est = ((-rate if side == "long" else rate) * notional) if settles else D(0)
        px = (pc - po) * qty if side == "long" else (po - pc) * qty
        legs[side] = dict(ex=ex, qty=qty, po=po, pc=pc, notional=notional, rate=rate, settles=settles, est=est, px=px,
                          fo=D(o["fee"]), fc=D(c["fee"]), real=D(legs_pnl[ex]["components"]["funding"]),
                          missing_funding="Funding" in legs_pnl[ex]["missing"], interval=s["funding_interval_secs"])
    L, S = legs["long"], legs["short"]

    out = [f"### {short_name}  `{pid}`", ""]
    # ---- A. basic info and timeline
    hold = (fills[("long", "close")][0] - fills[("long", "open")][0]) / 1000
    out += ["**A. 基本資訊與時間線（UTC）**", "", "| 項目 | 內容 |", "|---|---|"]
    out += [f"| 結算時間 | {utc(settle_ms)}（Bybit 週期 {L['interval'] // 3600}h／Binance 週期 {S['interval'] // 3600}h） |"]
    out += [f"| 目標名目／槓桿 | {snap['notional_usdt']} USDT／{snap['leverage']}x |"]
    out += [f"| T-10 快照（進場檢查） | {utc(check[0])}（距結算 {(settle_ms - check[0]) / 1000:.1f} 秒） |"]
    out += [f"| 進場成交 | {utc(fills[('long', 'open')][0])} |"]
    out += [f"| 出場成交 | {utc(fills[('long', 'close')][0])}（持倉約 {hold:.0f} 秒，跨結算） |"]
    if closed_ts:
        out += [f"| 確認雙邊平倉 | {utc(closed_ts)} |"]
    for side in ("long", "short"):
        for action in ("open", "close"):
            l = lat.get((side, action))
            if l:
                out += [f"| 下單延遲 {legs[side]['ex']} {'進' if action == 'open' else '出'}場 | {l['latency_ms']} ms（觸發到回應 {l['trigger_to_ack_ms']} ms） |"]
    for ex, (ts, p) in linked.items():
        out += [f"| {ex} 資金費流水入庫 | {utc(ts)}（結算後 {p['delay_ms'] / 1000:.0f} 秒）金額 {sg(p['amount'])} |"]
    out += [f"| 損益狀態 | {pnl['status']} |", ""]

    # ---- B. per-leg stage table
    fo, fc = L["fo"] + S["fo"], L["fc"] + S["fc"]
    est_sum, real_sum, px_sum = L["est"] + S["est"], L["real"] + S["real"], L["px"] + S["px"]
    note = lambda x: "（本次無結算）" if not x["settles"] else ""
    rows = [
        ("1 成交數量", f"{L['qty']}", f"{S['qty']}", ""),
        ("2 進場價 → 出場價", f"{L['po']} → {L['pc']}", f"{S['po']} → {S['pc']}", ""),
        ("3 名目價值（數量×進場價）", f"{L['notional']:.2f}", f"{S['notional']:.2f}", f"{L['notional'] + S['notional']:.2f}"),
        ("資金費率（T-10；正＝多單付、空單收）", pct(L["rate"]) + note(L), pct(S["rate"]) + note(S), ""),
        ("4 進場手續費（real）", cost(L["fo"]), cost(S["fo"]), cost(fo)),
        ("5 資金費推算（T-10 費率×名目）", sg(L["est"]), sg(S["est"]), sg(est_sum)),
        ("6 資金費 real（帳戶流水）", sg(L["real"]) + ("（缺流水）" if L["missing_funding"] else ""),
         sg(S["real"]) + ("（缺流水）" if S["missing_funding"] else "") + (f"（demo 費率 {demo_rate(symbol, settle_ms)}）" if S["ex"] == "Binance" and S["settles"] else ""), sg(real_sum)),
        ("7 出場手續費（real）", cost(L["fc"]), cost(S["fc"]), cost(fc)),
        ("8 總成本（4＋7）", cost(L["fo"] + L["fc"]), cost(S["fo"] + S["fc"]), cost(fo + fc)),
        ("價差損益（real）", sg(L["px"]), sg(S["px"]), sg(px_sum)),
    ]
    ge, gr = {k: legs[k]["est"] + legs[k]["px"] for k in legs}, {k: legs[k]["real"] + legs[k]["px"] for k in legs}
    cs = {k: legs[k]["fo"] + legs[k]["fc"] for k in legs}
    ne, nr = {k: ge[k] - cs[k] for k in legs}, {k: gr[k] - cs[k] for k in legs}
    rows += [
        ("9a 推算總獲益（5＋價差損益）", sg(ge["long"]), sg(ge["short"]), sg(ge["long"] + ge["short"])),
        ("9b 實際總獲益（6＋價差損益）", sg(gr["long"]), sg(gr["short"]), sg(gr["long"] + gr["short"])),
        ("10a **推算淨損益**（9a−8）", sg(ne["long"]), sg(ne["short"]), f"**{sg(ne['long'] + ne['short'])}**"),
        ("10b **實際淨損益**（9b−8）", sg(nr["long"]), sg(nr["short"]), f"**{sg(nr['long'] + nr['short'])}**"),
    ]
    out += ["**B. 各階段明細**", "", f"| 階段 | 多單 {L['ex']} | 空單 {S['ex']} | 雙邊加總 |", "|---|---|---|---|"]
    out += [f"| {a} | {b} | {c} | {d} |" for a, b, c, d in rows]
    out += [""]

    # ---- C. summary and self-check
    actual = nr["long"] + nr["short"]
    db_net = D(pnl["breakdown"]["total"]["net"])
    if pnl["status"] == "COMPLETE" and abs(actual - db_net) > D("0.0001"):
        sys.exit(f"SELF-CHECK FAILED for {pid}: computed actual net {actual} != database net {db_net}")
    out += ["**C. 總結**", "", "| 項目 | 金額 |", "|---|---|"]
    out += [f"| 進場預估淨邊際（系統進場前算的） | {sg(snap['net_edge']['net_edge_usdt'])} |"]
    out += [f"| 資金費合計：推算／實際 | {sg(est_sum)}／{sg(real_sum)} |"]
    out += [f"| 價差損益合計 | {sg(px_sum)} |"]
    out += [f"| 總成本（手續費） | {cost(fo + fc)} |"]
    out += [f"| 推算淨損益（10a） | {sg(ne['long'] + ne['short'])} |"]
    out += [f"| 實際淨損益（10b，與資料庫 PnL 事件一致：{sg(db_net)}） | {sg(actual)} |", ""]
    return "\n".join(out)


def main():
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    pairs = [r[0] for r in db.execute("select distinct pair_id from events where event_type='CLOSE_CONFIRMED' order by id")]
    keys = [k.lower() for k in sys.argv[1:]]
    if keys:
        pairs = [p for p in pairs if any(k in p.lower() for k in keys)]
    if not pairs:
        sys.exit("no matching closed pair")
    print("\n\n".join(pair_table(db, p) for p in pairs))


if __name__ == "__main__":
    main()

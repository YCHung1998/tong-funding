#!/usr/bin/env python3
"""Cost calibration report of finalized pairs, read from funding.db (read-only).

Usage:
    python3 issue/tools/cost_report.py
    TONG_FUNDING_DB=/path/to/funding.db python3 issue/tools/cost_report.py

Contract: openspec/changes/cost-attribution (spec "校準報表為唯讀、如實標示限制、只給建議").

Per FINALIZED pair only the newest PAIR_PNL_COMPUTED / PAIR_PNL_RECOMPUTED event is used. Its `breakdown` gives every
item except two values, which its `attribution` ({"entry_slippage", "notional_per_leg"}, decimal strings or null) gives:
  drift          = breakdown.total.price_ref
  total slippage = breakdown.total.slippage            exit slippage = total slippage - entry slippage
  fees           = total.opening_fee + total.closing_fee            funding, net = total.funding, total.net
  percentages    = USDT / notional_per_leg x 100
An item whose component is listed in the pair-level breakdown.missing is unknown (None, printed as a dash), never 0.

An event without `attribution` (written before the engine produced it) is restored from the raw events with the engine's
rules and marked 還原值: the entry reference is expected_price of the LAST ORDER_SUBMIT snapshot (never the mark price);
opening fills are the last (cumulative) ORDER_FILL of each client_order_id, summed per leg; a leg without an opening fill
contributes 0; an OKX leg or a missing price makes both values unknown.
  entry slippage = sum dir x (ref - act) x q  (long -1, short +1; positive = adverse)
  notional/leg   = mean of the opening fill value (q x act) of the legs that opened

The database is opened with mode=ro and every query runs in one read transaction. A closed WAL database (the app deletes
-wal / -shm on a clean close, after which a mode=ro open fails) is read as an immutable snapshot, which the report says,
and the read is abandoned when the file changed meanwhile.
"""
import json
import os
import sqlite3
import sys
from collections import namedtuple
from decimal import Decimal as D, InvalidOperation
from pathlib import Path

DB = os.environ.get("TONG_FUNDING_DB") or os.path.expanduser("~/Library/Application Support/tong-funding/funding.db")
MINUS = "−"
DASH = "—"
MIN_SAMPLES = 30
PERCENTILES = (50, 75, 90)
PNL_EVENTS = ("PAIR_PNL_COMPUTED", "PAIR_PNL_RECOMPUTED")
SHARE_LABELS = ("進場滑價", "出場滑價", "不利漂移", "手續費")
SIDE_FILES = ("-wal", "-shm")
SNAPSHOT_NOTE = "以快照方式讀取（App 未執行）"
HEADER = "【說明】demo 環境的數字不代表正式環境；建議值僅供參考，不會自動套用到任何設定。"
NOTES = (
    "- 參考價是正式環境的 mark 價：開倉＝下單前重抓、平倉＝平倉前重抓；漂移是兩所正式 mark 價在持倉期間的相對變化。",
    "- demo 資料庫的成交發生在 demo 市場，滑價＝demo 成交價 − 正式 mark 價，含 demo 與正式盤的價差，不可當作正式環境的滑價；只有 Live 的資料能用來校準滑價。",
)

# rows: one dict per used pair; skipped: [(pair id, reason)]; finalized: how many pairs reached FINALIZED
Loaded = namedtuple("Loaded", "rows skipped finalized")


class Unreadable(Exception):
    """One pair's events do not read as the contract says: the pair is skipped and listed with this reason."""


class SnapshotChanged(Exception):
    """The database changed while it was read as an immutable snapshot."""


class ReadOnlyDb(sqlite3.Connection):
    """A read-only connection and how it was opened: `immutable` = read as a closed database's snapshot,
    with the main file's (size, mtime_ns) from before the read."""

    immutable = False
    path = None
    before = None


# ---------- opening the database ----------
def side_files(path):
    return [path + s for s in SIDE_FILES if os.path.exists(path + s)]


def file_state(path):
    st = os.stat(path)
    return st.st_size, st.st_mtime_ns


def open_db(path, options, immutable):
    # as_uri() percent-encodes the path: a '#', '?' or '%' in a directory name would otherwise cut the URI short,
    # drop "?mode=ro" and let SQLite open (and create) another file read-write
    db = sqlite3.connect(Path(path).resolve().as_uri() + options, uri=True, isolation_level=None, factory=ReadOnlyDb)
    db.immutable, db.path = immutable, path
    try:
        db.execute("BEGIN")  # every query of the report reads the same snapshot
        db.execute("select 1 from sqlite_master limit 1").fetchall()  # the first query: where a closed WAL database fails
    except sqlite3.Error:
        db.close()
        raise
    return db


def connect(path):
    """Read-only. The app keeps the database in WAL mode and its SQLite deletes -wal / -shm on a clean close; a mode=ro
    connection to such a closed database fails at its first query with "unable to open database file" (observed, and it
    creates no file). Only that failure, with neither -wal nor -shm present (nothing has the database open), is read
    again as immutable. Anything else raises."""
    os.stat(path)  # a missing file raises FileNotFoundError; it must not look like a closed database
    try:
        return open_db(path, "?mode=ro", False)
    except sqlite3.OperationalError as e:
        if "unable to open database file" not in str(e) or side_files(path):
            raise
    before = file_state(path)
    db = open_db(path, "?mode=ro&immutable=1", True)
    db.before = before
    return db


def finish(db):
    """End the read transaction. A snapshot read must also prove that nothing changed while it ran."""
    if db.in_transaction:
        db.execute("COMMIT")
    if not db.immutable:
        return
    problems = []
    try:
        if file_state(db.path) != db.before:
            problems.append("主檔的大小或修改時間變了")
    except FileNotFoundError:
        problems.append("主檔不見了")
    appeared = [os.path.basename(f) for f in side_files(db.path)]
    if appeared:
        problems.append("出現了 " + " / ".join(appeared) + "（App 已啟動）")
    if problems:
        raise SnapshotChanged("讀取期間資料庫被改動（" + "；".join(problems) + "），這份報表不可信，請重跑一次。")


# ---------- reading the events ----------
def dec(value, name):
    """Decimal of a payload number (string or number). None stays None; anything else is Unreadable."""
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (str, int, float)):
        raise Unreadable(f"{name} 不是十進位數：{value!r}")
    try:
        d = D(str(value))
    except InvalidOperation:
        raise Unreadable(f"{name} 不是十進位數：{value!r}") from None
    if not d.is_finite():
        raise Unreadable(f"{name} 不是有限的數：{value!r}")
    return d


def obj(x):
    return x if isinstance(x, dict) else {}


def pair_events(db, pid):
    """Every event of a pair, oldest first: (id, event_type, payload)."""
    events = []
    for id_, kind, payload in db.execute("select id, event_type, payload from events where pair_id = ? order by id", (pid,)):
        p = json.loads(payload)
        if not isinstance(p, dict):
            raise Unreadable(f"事件 {id_}（{kind}）的 payload 不是物件")
        events.append((id_, kind, p))
    return events


def entry_snapshot(events):
    """entry_snapshot of the LAST ORDER_SUBMIT transition, as the engine reads it; None when that has none."""
    last = None
    for _, kind, p in events:
        if kind == "PAIR_TRANSITION" and p.get("to") == "ORDER_SUBMIT":
            last = p
    snap = obj(obj(last).get("detail")).get("entry_snapshot")
    return snap if isinstance(snap, dict) else None


def restore(events):
    """(entry_slippage, notional_per_leg, why) of a pair whose PnL event has no `attribution`, from the raw events with the
    engine's rules. Both are None, with the reason, when any opening fill cannot be fully priced: the spec nulls them together."""
    snap = entry_snapshot(events)
    orders = {}  # client_order_id -> its last ORDER_FILL; fills are cumulative, so the last one is the order's fill
    for _, kind, p in events:
        if kind == "ORDER_FILL" and p.get("action") == "open" and p.get("leg") in ("long", "short") and p.get("client_order_id"):
            orders[p["client_order_id"]] = p
    entry, notional, problems = D(0), {}, []
    for cid, p in orders.items():
        leg = p["leg"]
        q = dec(p.get("filled_quantity"), f"{cid} 的 filled_quantity") or D(0)
        if q == 0:
            continue  # nothing filled: this leg has no trade and contributes 0
        leg_snap = obj(obj(snap).get(leg))
        if any(isinstance(x, str) and x.strip().upper() == "OKX" for x in (p.get("exchange"), leg_snap.get("exchange"))):
            problems.append(f"{leg} 腿是 OKX（成交數量是合約張數，這裡不換算）")
            continue
        ref = dec(leg_snap.get("expected_price"), f"進場快照 {leg}.expected_price")
        act = dec(p.get("avg_price"), f"{cid} 的 avg_price")
        if ref is None:
            problems.append(f"進場快照沒有 {leg}.expected_price（不以 mark_price 代替）")
        if act is None:
            problems.append(f"開倉成交 {cid} 沒有 avg_price")
        if ref is None or act is None:
            continue
        entry += (-1 if leg == "long" else 1) * (ref - act) * q
        notional[leg] = notional.get(leg, D(0)) + act * q
    if problems:
        return None, None, "；".join(dict.fromkeys(problems))
    if not notional:
        return None, None, "沒有任何開倉成交"
    mean = sum(notional.values(), D(0)) / len(notional)
    if mean <= 0:
        return None, None, "每腿名目不是正數"
    return entry, mean, None


def breakdown_of(pnl):
    b = pnl.get("breakdown")
    if not isinstance(b, dict) or not isinstance(b.get("total"), dict) or not isinstance(b.get("missing"), list):
        raise Unreadable("PnL 事件沒有 breakdown.total 或 breakdown.missing")
    if not all(isinstance(m, str) for m in b["missing"]):
        raise Unreadable("breakdown.missing 不是元件名稱的清單")
    return b["total"], set(b["missing"])


def backtest_inputs(snap):
    """What the entry snapshot gives to recompute Net Edge with other settings; None = this pair cannot be back-tested."""
    ne = obj(snap).get("net_edge")
    if not isinstance(ne, dict):
        return None
    try:
        values = [dec(snap.get("notional_usdt"), "notional_usdt"), dec(ne.get("funding_income_usdt"), "net_edge.funding_income_usdt"),
                  dec(ne.get("fee_usdt"), "net_edge.fee_usdt"), dec(ne.get("threshold_pct"), "net_edge.threshold_pct")]
    except Unreadable:
        return None
    if any(v is None for v in values) or values[0] <= 0:
        return None
    return dict(zip(("notional", "funding_income", "fee", "threshold"), values))


def build_row(pid, events, pnl):
    total, missing = breakdown_of(pnl)
    att = pnl.get("attribution")
    if att is None:
        entry, notional, why = restore(events)
    elif isinstance(att, dict):
        entry = dec(att.get("entry_slippage"), "attribution.entry_slippage")
        notional = dec(att.get("notional_per_leg"), "attribution.notional_per_leg")
        why = "attribution.notional_per_leg 為 null"
    else:
        raise Unreadable("attribution 不是物件")
    if notional is None:
        raise Unreadable(f"每腿名目未知：{why}")
    if notional <= 0:
        raise Unreadable(f"每腿名目不是正數：{notional}")

    def known(key, *components):  # a component in the pair-level `missing` makes the item unknown
        if missing.intersection(components):
            return None
        if key not in total:
            raise Unreadable(f"breakdown.total 沒有 {key}")
        return dec(total[key], f"breakdown.total.{key}")

    slippage = known("slippage", "Slippage")
    opening, closing = known("opening_fee", "OpeningFee"), known("closing_fee", "ClosingFee")
    usdt = {
        "entry_slippage": entry,
        "slippage": slippage,
        "exit_slippage": None if slippage is None or entry is None else slippage - entry,
        "drift": known("price_ref", "PriceRef"),
        "fees": None if opening is None or closing is None else opening + closing,
        "funding": known("funding", "Funding"),
        "net": known("net", "Net"),
    }
    row = {"pid": pid, "restored": att is None, "notional_per_leg": notional, "bt": backtest_inputs(entry_snapshot(events)),
           "early_exit": any(kind == "LEG_EARLY_EXIT" for _, kind, _ in events)}
    for k, v in usdt.items():
        row[k] = {"usdt": v, "pct": None if v is None else v / notional * 100}
    return row


def load_pairs(db):
    finalized = {}
    for pid, payload in db.execute("select pair_id, payload from events where event_type = 'PAIR_TRANSITION' and payload like '%FINALIZED%' order by id"):
        p = json.loads(payload)
        if pid is not None and isinstance(p, dict) and p.get("to") == "FINALIZED":
            finalized[pid] = None
    rows, skipped = [], []
    for pid in finalized:
        try:
            events = pair_events(db, pid)
            pnls = [p for _, kind, p in events if kind in PNL_EVENTS]
            if not pnls:
                raise Unreadable("沒有 PnL 事件")
            rows.append(build_row(pid, events, pnls[-1]))  # only the newest PnL event of the pair
        except Unreadable as e:
            skipped.append((pid, str(e)))
    return Loaded(rows, skipped, len(finalized))


# ---------- statistics ----------
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


def cost_shares(rows):
    """Share of each cost in the total ADVERSE cost: per pair max(0, entry), max(0, exit), max(0, -drift) and max(0, fees)
    in % of the per-leg notional, over the pairs whose four items are all known. Returns (shares or None, pairs used)."""
    sums = dict.fromkeys(SHARE_LABELS, D(0))
    used = 0
    for r in rows:
        entry, exit_, drift, fees = (r[k]["pct"] for k in ("entry_slippage", "exit_slippage", "drift", "fees"))
        if any(v is None for v in (entry, exit_, drift, fees)):
            continue
        used += 1
        for label, cost in zip(sums, (entry, exit_, -drift, fees)):
            sums[label] += max(D(0), cost)
    total = sum(sums.values(), D(0))
    return ({k: v / total * 100 for k, v in sums.items()} if total else None), used


def suggest(rows):
    """Per percentile: the total-slippage quantile, est_slippage_pct (= quantile / 4, never below 0: the setting cannot
    be negative) and safety_margin_pct (= the adverse-drift quantile; a favourable drift counts 0)."""
    slippage = pcts(rows, "slippage")
    adverse = [max(D(0), -x) for x in pcts(rows, "drift")]
    out = {}
    for p in PERCENTILES:
        q = percentile(slippage, p)
        out[p] = {"slippage": q, "est": None if q is None else max(D(0), q) / 4, "clamped": q is not None and q < 0, "safety": percentile(adverse, p)}
    return out


def outcome(rows):
    """Wins (net > 0), losses (net <= 0) and the total net % of `rows`; a pair whose net is unknown is counted apart."""
    nets = [r["net"]["pct"] for r in rows]
    known = [n for n in nets if n is not None]
    return {"wins": sum(1 for n in known if n > 0), "losses": sum(1 for n in known if n <= 0),
            "unknown_net": len(nets) - len(known), "net_total": sum(known, D(0))}


def backtest(rows, est, safety):
    """Which pairs would still have been entered with this est_slippage_pct / safety_margin_pct, and what they really earned.
    Net Edge % = (funding income - fee - N x 4 x est / 100 - N x safety / 100) / N x 100 from the entry snapshot; it must
    reach the snapshot's threshold. Pairs whose snapshot lacks an input are `unable`."""
    entering, testable = [], 0
    for r in rows:
        b = r["bt"]
        if b is None:
            continue
        testable += 1
        n = b["notional"]
        edge = (b["funding_income"] - b["fee"] - n * 4 * est / 100 - n * safety / 100) / n * 100
        if edge >= b["threshold"]:
            entering.append(r)
    return {"enter": len(entering), "testable": testable, "unable": len(rows) - testable, **outcome(entering)}


# ---------- formatting ----------
def sg_pct(x):
    if x is None:
        return DASH
    return f"+{x:.4f}%" if x >= 0 else f"{MINUS}{abs(x):.4f}%"


def cost_pct(x):  # a cost: positive = paid, in parentheses; negative = favourable, shown as income
    if x is None:
        return DASH
    return f"({x:.4f}%)" if x >= 0 else f"+{abs(x):.4f}%"


def plain(x, suffix=""):
    return DASH if x is None else f"{x:.4f}{suffix}"


def header_lines(loaded, snapshot):
    n = len(loaded.rows)
    out = [SNAPSHOT_NOTE] if snapshot else []
    out += [HEADER, *NOTES, "", f"## 成本校準報表（樣本數 n={n}）", "", f"FINALIZED 總數 / 實際使用筆數：{loaded.finalized} / {n}", ""]
    if n < MIN_SAMPLES:
        out += [f"> 樣本不足，僅供參考（n={n} < {MIN_SAMPLES}）。", ""]
    out += [f"### 略過的配對（{len(loaded.skipped)} 筆）", ""]
    out += [f"- {pid}：{why}" for pid, why in loaded.skipped] or ["（無）"]
    return out + [""]


def table_lines(rows):
    out = ["### 逐筆拆解", "",
           "成本放括號、損益帶 +/− 符號：滑價與手續費為成本（括號內為不利，+ 為有利），漂移、資金費、淨為損益；百分比皆為每腿名目 %"
           "（每腿名目＝有開倉成交的各腿開倉成交金額的平均）。「—」＝資料不足，不是 0。出場滑價＝總滑價 − 進場滑價，漂移＝breakdown 的 price_ref。"
           "進場滑價與每腿名目取自 attribution；來源「還原值」＝最新 PnL 事件沒有 attribution，這兩項由原始事件還原，其餘項目一律取自 breakdown。"
           "提前出場＝該配對有 LEG_EARLY_EXIT 事件。", "",
           "| 配對 | 來源 | 提前出場 | 進場滑價% | 漂移% | 出場滑價% | 手續費% | 資金費% | 淨% |", "|---|---|---|---|---|---|---|---|---|"]
    for r in rows:
        out.append("| {} | {} | {} | {} | {} | {} | {} | {} | {} |".format(
            r["pid"], "還原值" if r["restored"] else "attribution", "是" if r["early_exit"] else "否",
            cost_pct(r["entry_slippage"]["pct"]), sg_pct(r["drift"]["pct"]), cost_pct(r["exit_slippage"]["pct"]),
            cost_pct(r["fees"]["pct"]), sg_pct(r["funding"]["pct"]), sg_pct(r["net"]["pct"])))
    return out + [""]


def shares_lines(rows):
    shares, used = cost_shares(rows)
    out = ["### 各項佔不利成本比例", "",
           "逐筆只計不利部分：max(0, 進場滑價)、max(0, 出場滑價)、max(0, −漂移)、max(0, 手續費)，以每腿名目 % 加總；"
           f"只納入四項皆已知的配對（{used} 筆，其餘 {len(rows) - used} 筆有未知項目而不納入）。", "",
           "| 項目 | 佔比 |", "|---|---|"]
    out += [f"| {k} | {DASH if shares is None else format(shares[k], '.1f') + '%'} |" for k in SHARE_LABELS]
    return out + [""]


def quantile_lines(rows, s):
    out = ["### 分位數（每腿名目 %）", "", "| 項目 | p50 | p75 | p90 | 樣本 |", "|---|---|---|---|---|"]
    for label, key, count in (("總滑價（進場＋出場）", "slippage", len(pcts(rows, "slippage"))),
                              ("不利漂移（漂移為負時的絕對值，有利記 0）", "safety", len(pcts(rows, "drift")))):
        out.append(f"| {label} | " + " | ".join(plain(s[p][key], "%") for p in PERCENTILES) + f" | {count} |")
    return out + [""]


def suggestion_lines(s):
    return ["### 建議值（只輸出，不會自動套用）", "",
            "est_slippage_pct = 總滑價分位數 ÷ 4（Net Edge 以 4 次成交計；分位數為負時以 0 計，該設定不可為負）；safety_margin_pct = 不利漂移分位數。", "",
            "| 設定 | p50 | p75 | p90 |", "|---|---|---|---|",
            "| est_slippage_pct | " + " | ".join(plain(s[p]["est"]) + ("（分位數為負，以 0 計）" if s[p]["clamped"] else "") for p in PERCENTILES) + " |",
            "| safety_margin_pct | " + " | ".join(plain(s[p]["safety"]) for p in PERCENTILES) + " |", ""]


def backtest_lines(rows, s):
    n = len(rows)
    out = ["### 樣本內回測（用進場快照重算 Net Edge）", "",
           "對每組建議值，用各配對最新進場快照的 notional_usdt（N）、net_edge.funding_income_usdt、net_edge.fee_usdt、net_edge.threshold_pct 重算 "
           "net_edge_pct = (funding_income − fee − N×4×est/100 − N×safety/100) ÷ N × 100，≥ threshold_pct 視為仍會進場。"
           "勝＝實際淨損益 > 0，負＝≤ 0；實際淨損益為每腿名目 % 的合計。結果只代表這份樣本，不是對未來的預測；合計為負時標「樣本內仍為負」。", "",
           "| 設定 | est_slippage_pct | safety_margin_pct | 仍會進場 / 可回測 | 勝 / 負 | 實際淨損益合計 | 備註 |", "|---|---|---|---|---|---|---|"]
    for p in PERCENTILES:
        est, safety = s[p]["est"], s[p]["safety"]
        if est is None or safety is None:
            out.append(f"| p{p} | {DASH} | {DASH} | {DASH} | {DASH} | {DASH} | 沒有樣本 |")
            continue
        b = backtest(rows, est, safety)
        notes = (["樣本內仍為負"] if b["enter"] and b["net_total"] < 0 else []) + (
            [f"淨損益未知 {b['unknown_net']} 筆，不計入勝負與合計"] if b["unknown_net"] else [])
        out.append(f"| p{p} | {est:.4f} | {safety:.4f} | {b['enter']} / {b['testable']} | {b['wins']} / {b['losses']} | "
                   f"{sg_pct(b['net_total']) if b['enter'] else DASH} | {'；'.join(notes)} |")
    a = outcome(rows)  # the settings the pairs were really entered with: all of them
    out.append(f"| 實際進場時的設定 | 各筆當時的設定 | 各筆當時的設定 | {n} / {n} | {a['wins']} / {a['losses']} | {sg_pct(a['net_total']) if n else DASH} | "
               + (f"淨損益未知 {a['unknown_net']} 筆，不計入勝負與合計" if a["unknown_net"] else "") + " |")
    unable = sum(1 for r in rows if r["bt"] is None)
    if unable:
        out += ["", f"無法回測 {unable} 筆（進場快照缺 notional_usdt、net_edge.funding_income_usdt、net_edge.fee_usdt 或 net_edge.threshold_pct）。"]
    return out + [""]


def render_report(loaded, snapshot=False):
    rows, n = loaded.rows, len(loaded.rows)
    s = suggest(rows)
    out = header_lines(loaded, snapshot) + table_lines(rows) + shares_lines(rows) + quantile_lines(rows, s) + suggestion_lines(s) + backtest_lines(rows, s)
    out.append(f"樣本數 n={n}" + ("；樣本不足，僅供參考。" if n < MIN_SAMPLES else "。"))
    return "\n".join(out + [""])


def main():
    db = connect(DB)
    try:
        loaded = load_pairs(db)
        finish(db)
    except SnapshotChanged as e:
        sys.exit(f"cost_report: {e}")
    finally:
        db.close()
    print(render_report(loaded, snapshot=db.immutable))
    if not loaded.rows:
        sys.exit("cost_report: 沒有可用的已平倉配對（原因見上方「略過的配對」）")


if __name__ == "__main__":
    main()

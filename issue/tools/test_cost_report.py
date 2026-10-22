"""Tests of cost_report.py on synthetic databases (no network, no real data).

Run:  python3 -m unittest issue/tools/test_cost_report.py -v     (from the repository root)

Synthetic pair (qty 10): long  open ref 100 act 100.1, close ref 100.5 act 100.4
                         short open ref 100.2 act 100.1, close ref 100.6 act 100.7
  entry slippage 1.0 + 1.0 = 2.0 ; exit slippage 1.0 + 1.0 = 2.0 ; slippage 4.0
  drift = (-1000 + 1005) + (1002 - 1006) = +1.0 ; notional per leg = (1001 + 1001) / 2 = 1001
  fees 0.05 x 4 = 0.2 ; funding 0.3 ; net = 0.3 + 1.0 - 4.0 - 0.2 = -2.9
"""
import hashlib
import importlib.util
import io
import json
import os
import sqlite3
import tempfile
import unittest
from contextlib import redirect_stdout
from decimal import Decimal as D
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("cost_report", os.path.join(HERE, "cost_report.py"))
cr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cr)

S = 1_800_000_000_000
# (leg, open ref, open act, close ref, close act)
LEGS = {"long": ("100", "100.1", "100.5", "100.4"), "short": ("100.2", "100.1", "100.6", "100.7")}
ATTRIBUTION = {
    "notional_per_leg": "1001",
    "funding": {"usdt": "0.3", "pct": "0.02997"},
    "drift": {"usdt": "1.0", "pct": "0.09990"},
    "entry_slippage": {"usdt": "2.0", "pct": "0.19980"},
    "exit_slippage": {"usdt": "2.0", "pct": "0.19980"},
    "fees": {"usdt": "0.2", "pct": "0.01998"},
    "net": {"usdt": "-2.9", "pct": "-0.28971"},
    "unknown": [],
    "reference_sources": {"open": "mark(entry_snapshot)", "close": "refetch_before_close"},
}


def schema(db):
    db.execute("create table events (id integer primary key autoincrement, ts_ms integer, event_type text, pair_id text, payload text)")


def add_pair(db, pid, legs=LEGS, attribution=None, close_ref=True, scale=1, expected=None):
    def ev(ts, kind, payload):
        db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (?,?,?,?)", (ts, kind, pid, json.dumps(payload)))

    snap = {side: {"exchange": "Bybit" if side == "long" else "Binance", "mark_price": legs[side][0]} for side in legs}
    for side, price in (expected or {}).items():
        snap[side]["expected_price"] = price
    ev(S - 9_000, "PAIR_TRANSITION", {"from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": {"entry_snapshot": snap}})
    for side, (_, ao, rc, ac) in ((s, (None, v[1], v[2], v[3])) for s, v in legs.items()):
        ev(S - 8_000, "ORDER_FILL", {"leg": side, "action": "open", "filled_quantity": "10", "avg_price": ao, "fee": "0.05"})
        sub = {"leg": side, "action": "close"}
        if close_ref:
            sub["reference_price"] = rc
        ev(S + 15_000, "ORDER_SUBMITTED", sub)
        ev(S + 16_000, "ORDER_FILL", {"leg": side, "action": "close", "filled_quantity": "10", "avg_price": ac, "fee": "0.05"})
    ev(S + 17_000, "PAIR_TRANSITION", {"from": "CLOSING", "to": "FINALIZED"})
    payload = {"status": "COMPLETE", "breakdown": {"legs": [
        {"exchange": "Bybit", "components": {"funding": "0.2"}, "missing": []},
        {"exchange": "Binance", "components": {"funding": "0.1"}, "missing": []}], "total": {"net": "-2.9"}}}
    if attribution:
        payload["attribution"] = attribution
    ev(S + 20_000, "PAIR_PNL_COMPUTED", payload)


def make_db(path=":memory:", wal=False, **kw):
    db = sqlite3.connect(path)
    if wal:
        db.execute("pragma journal_mode=wal")
    schema(db)
    add_pair(db, "p-1", **kw)
    db.commit()
    return db


class CostReport(unittest.TestCase):
    def test_identity_entry_plus_exit_is_total_slippage(self):
        row = cr.load_pairs(make_db())[0]
        self.assertTrue(row["restored"])
        self.assertEqual(row["entry_slippage"]["usdt"], D("2.0"))
        self.assertEqual(row["exit_slippage"]["usdt"], D("2.0"))
        self.assertEqual(row["entry_slippage"]["usdt"] + row["exit_slippage"]["usdt"], D("4.0"))
        self.assertEqual(row["drift"]["usdt"], D("1.0"))
        self.assertEqual(row["notional_per_leg"], D("1001"))
        n = row["funding"]["usdt"] + row["drift"]["usdt"] - row["entry_slippage"]["usdt"] - row["exit_slippage"]["usdt"] - row["fees"]["usdt"]
        self.assertEqual(n, row["net"]["usdt"])
        self.assertEqual(row["net"]["usdt"], D("-2.9"))

    def test_missing_reference_is_unknown_not_zero(self):
        row = cr.load_pairs(make_db(close_ref=False))[0]
        self.assertIsNone(row["exit_slippage"]["usdt"])
        self.assertIsNone(row["drift"]["usdt"])
        self.assertIsNone(row["net"]["usdt"])
        self.assertEqual(row["entry_slippage"]["usdt"], D("2.0"))
        text = cr.render_report([row])
        line = next(l for l in text.splitlines() if l.startswith("| p-1"))
        self.assertIn("—", line)
        self.assertNotIn("(0.0000%)", line)

    def test_attribution_and_restored_agree(self):
        a = cr.load_pairs(make_db(attribution=ATTRIBUTION))[0]
        r = cr.load_pairs(make_db())[0]
        self.assertFalse(a["restored"])
        self.assertTrue(r["restored"])
        for k in ("entry_slippage", "exit_slippage", "drift", "fees", "funding", "net"):
            self.assertEqual(a[k]["usdt"], r[k]["usdt"], k)
        self.assertEqual(a["notional_per_leg"], r["notional_per_leg"])
        self.assertAlmostEqual(float(a["drift"]["pct"]), float(r["drift"]["pct"]), places=3)
        rowline = lambda row: next(l for l in cr.render_report([row]).splitlines() if l.startswith("| p-1"))
        self.assertNotIn("還原值", rowline(a))
        self.assertIn("還原值", rowline(r))

    def test_small_sample_warning_and_header(self):
        text = cr.render_report([cr.load_pairs(make_db())[0]])
        self.assertIn("樣本不足，僅供參考", text)
        self.assertIn("n=1", text)
        self.assertIn("demo", text.splitlines()[0])
        self.assertIn("不會自動套用", text.splitlines()[0])

    def test_no_warning_at_30(self):
        row = cr.load_pairs(make_db())[0]
        text = cr.render_report([row] * 30)
        self.assertNotIn("樣本不足", text)
        self.assertIn("n=30", text)

    def test_percentile_and_suggestions(self):
        self.assertEqual(cr.percentile([D(1), D(2), D(3), D(4), D(5)], 50), D(3))
        self.assertEqual(cr.percentile([D(0), D(10)], 75), D("7.5"))
        row = cr.load_pairs(make_db())[0]
        text = cr.render_report([row])
        # total slippage 4.0 / 1001 = 0.3996% ; /4 = 0.0999%
        self.assertIn("est_slippage_pct", text)
        self.assertIn("0.0999", text)
        self.assertIn("safety_margin_pct", text)

    def test_read_only(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            make_db(path).close()
            def digest():
                with open(path, "rb") as f:
                    return hashlib.sha256(f.read()).hexdigest()

            before = digest()
            db = cr.connect(path)
            with self.assertRaises(sqlite3.OperationalError):
                db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (1,'X','p','{}')")
            db.close()
            with mock.patch.object(cr, "DB", path), redirect_stdout(io.StringIO()) as out:
                cr.main()
            self.assertIn("p-1", out.getvalue())
            self.assertEqual(before, digest())

    def test_entry_reference_is_the_snapshot_expected_price_like_the_engine(self):
        # the engine's open reference is entry_snapshot.<leg>.expected_price; mark_price is only a fallback
        row = cr.load_pairs(make_db(expected={"long": "100.05", "short": "100.2"}))[0]
        # long open: -1 x (100.05 - 100.1) x 10 = 0.5 ; short open: +1 x (100.2 - 100.1) x 10 = 1.0
        self.assertEqual(row["entry_slippage"]["usdt"], D("1.5"))

    def test_a_closed_wal_database_still_opens_read_only(self):
        # the app keeps funding.db in WAL mode; once it is closed the -wal / -shm files are gone and a
        # plain mode=ro open fails ("unable to open database file")
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            db = make_db(path, wal=True)
            db.execute("pragma wal_checkpoint(TRUNCATE)")
            db.close()
            # the app's bundled SQLite deletes both files on a clean close; macOS's system SQLite (used
            # by Python here) keeps them, so remove them to get the state the app leaves behind
            for ext in ("-wal", "-shm"):
                if os.path.exists(path + ext):
                    os.remove(path + ext)
            with open(path, "rb") as f:
                before = hashlib.sha256(f.read()).hexdigest()
            db = cr.connect(path)
            self.assertEqual(len(cr.load_pairs(db)), 1)
            with self.assertRaises(sqlite3.OperationalError):
                db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (1,'X','p','{}')")
            db.close()
            with open(path, "rb") as f:
                self.assertEqual(before, hashlib.sha256(f.read()).hexdigest())


if __name__ == "__main__":
    unittest.main()

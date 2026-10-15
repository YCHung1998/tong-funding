"""Tests of trade_table.py on a synthetic database (no network, no real data).

Run:  python3 -m unittest issue/tools/test_trade_table.py -v     (from the repository root)

Numbers of the synthetic pair (qty 100; long Bybit, short Binance; settlement S):
  long : open 1.0 close 0.9 -> price pnl -10.0, fees 0.5 + 0.5, T-10 rate -0.001 -> estimate +0.10, real +0.12
  short: open 1.0 close 1.0 -> price pnl  0.0, fees 0.4 + 0.4, T-10 rate -0.0005 -> estimate -0.05, real -0.04
  actual net = (0.12 - 10 - 1.0) + (-0.04 + 0 - 0.8) = -11.72 ; estimated net = (0.10 - 11.0) + (-0.05 - 0.8) = -11.75
"""
import importlib.util
import json
import os
import sqlite3
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("trade_table", os.path.join(HERE, "trade_table.py"))
tt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tt)

S = 1_800_000_000_000
PID = "ui-1-0-testusdt"


def make_db(net):
    db = sqlite3.connect(":memory:")
    db.execute("create table events (id integer primary key autoincrement, ts_ms integer, event_type text, pair_id text, payload text)")

    def ev(ts, kind, payload):
        db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (?,?,?,?)", (ts, kind, PID, json.dumps(payload)))

    snap = {
        "notional_usdt": "100", "leverage": "5",
        "long": {"exchange": "Bybit", "funding_rate": "-0.001", "funding_interval_secs": 28800, "next_funding_time": S},
        "short": {"exchange": "Binance", "funding_rate": "-0.0005", "funding_interval_secs": 14400, "next_funding_time": S},
        "net_edge": {"net_edge_usdt": "0.5"},
    }
    ev(S - 60_000, "PAIR_PREPARED", {"symbol": "TESTUSDT", "settlement_ms": S, "simulated": False})
    ev(S - 9_000, "PAIR_TRANSITION", {"from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": {"entry_snapshot": snap}})

    def fill(leg, action, price, fee, ts):
        ev(ts, "ORDER_FILL", {"leg": leg, "action": action, "filled_quantity": "100", "avg_price": price, "fee": fee})

    fill("long", "open", "1.0", "0.5", S - 8_000)
    fill("short", "open", "1.0", "0.4", S - 8_000)
    fill("long", "close", "0.9", "0.5", S + 16_000)
    fill("short", "close", "1.0", "0.4", S + 16_000)
    ev(S + 17_000, "CLOSE_CONFIRMED", {"verified_flat": True})
    leg = lambda ex, funding: {"exchange": ex, "components": {"funding": funding}, "missing": []}
    ev(S + 20_000, "PAIR_PNL_RECOMPUTED", {"status": "COMPLETE", "breakdown": {"legs": [leg("Bybit", "0.12"), leg("Binance", "-0.04")], "total": {"net": net}}})
    db.commit()
    return db


class TradeTable(unittest.TestCase):
    def render(self, net="-11.72"):
        with mock.patch.object(tt, "demo_rate", return_value="-0.0500%"):
            return tt.pair_table(make_db(net), PID)

    def test_actual_and_estimated_net_are_computed_from_the_definitions(self):
        out = self.render()
        self.assertIn("**−11.7200**", out)  # actual net (10b)
        self.assertIn("**−11.7500**", out)  # estimated net (10a)

    def test_costs_are_in_parentheses_and_income_and_loss_are_signed(self):
        out = self.render()
        self.assertIn("(0.5000)", out)      # entry fee of the long leg
        self.assertIn("(1.8000)", out)      # total cost of both legs
        self.assertIn("+0.1200", out)       # real funding of the long leg
        self.assertIn("−10.0000", out)      # price pnl of the long leg

    def test_funding_rate_is_shown_in_percent_with_its_sign(self):
        out = self.render()
        self.assertIn("−0.1000%", out)
        self.assertIn("−0.0500%", out)

    def test_a_mismatch_with_the_database_net_stops_the_script(self):
        with self.assertRaises(SystemExit) as cm:
            self.render(net="-9.99")
        self.assertIn("SELF-CHECK FAILED", str(cm.exception))

    def test_a_leg_without_a_settlement_at_this_time_has_zero_estimated_funding(self):
        db = make_db("-11.72")
        row = db.execute("select id, payload from events where event_type='PAIR_TRANSITION'").fetchone()
        p = json.loads(row[1])
        p["detail"]["entry_snapshot"]["short"]["next_funding_time"] = S + 8 * 3_600_000
        db.execute("update events set payload=? where id=?", (json.dumps(p), row[0]))
        with mock.patch.object(tt, "demo_rate", return_value="-"):
            out = tt.pair_table(db, PID)
        self.assertIn("（本次無結算）", out)
        self.assertIn("**−11.7000**", out)  # estimate: long +0.10 - 11.0, short 0 - 0.8


if __name__ == "__main__":
    unittest.main()

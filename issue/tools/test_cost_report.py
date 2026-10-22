"""Tests of cost_report.py on synthetic databases (no network, no real data).

Run:  python3 -m unittest issue/tools/test_cost_report.py -v     (from the repository root)

Standard pair (qty 10): long  open ref 100 act 100.1, close ref 100.5 act 100.4
                        short open ref 100.2 act 100.1, close ref 100.6 act 100.7
  entry slippage 1.0 + 1.0 = 2.0 ; exit slippage 1.0 + 1.0 = 2.0 ; total slippage 4.0
  drift = (-1000 + 1005) + (1002 - 1006) = +1.0 ; notional per leg = (1001 + 1001) / 2 = 1001
  fees 0.05 x 4 = 0.2 ; funding 0.3 ; net = 0.3 + 1.0 - 4.0 - 0.2 = -2.9
The PnL event carries the engine's `breakdown` (total + pair-level `missing`); a new event also carries
`attribution` = {entry_slippage, notional_per_leg}. The report reads everything else from the breakdown.
"""
import hashlib
import importlib.util
import io
import itertools
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

TS = itertools.count(1_800_000_000_000, 1000)
STANDARD_ATTRIBUTION = {"entry_slippage": "2.0", "notional_per_leg": "1001"}
STANDARD_OPENS = [("long", "lo", "10", "100.1"), ("short", "so", "10", "100.1")]


# ---------- synthetic database ----------
def schema(db):
    db.execute("create table events (id integer primary key autoincrement, ts_ms integer, event_type text, pair_id text, payload text)")


def ev(db, pid, kind, payload):
    db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (?,?,?,?)", (next(TS), kind, pid, json.dumps(payload)))


def snapshot(db, pid, long_ref="100", short_ref="100.2", long_ex="Bybit", short_ex="Binance", notional="1000",
             funding_income="1.0", fee="0.6", threshold="0.0021", edge=True):
    """The ORDER_SUBMIT transition with the entry snapshot. mark_price differs from expected_price on purpose."""
    snap = {"long": {"exchange": long_ex, "mark_price": "100.5"}, "short": {"exchange": short_ex, "mark_price": "100.9"}, "notional_usdt": notional}
    if long_ref is not None:
        snap["long"]["expected_price"] = long_ref
    if short_ref is not None:
        snap["short"]["expected_price"] = short_ref
    snap["net_edge"] = ({"net_edge_pct": "0.04", "funding_income_usdt": funding_income, "fee_usdt": fee, "slippage_usdt": "0.2",
                         "safety_margin_usdt": "0", "threshold_pct": threshold} if edge else {"unavailable": "no funding data"})
    ev(db, pid, "PAIR_TRANSITION", {"from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": {"entry_snapshot": snap}})


def fill(db, pid, leg, cid, qty, act, action="open", exchange=None):
    ev(db, pid, "ORDER_FILL", {"state": "Filled", "filled_quantity": qty, "avg_price": act, "fee": "0.05", "fee_asset": "USDT", "client_order_id": cid,
                               "exchange": exchange or ("Bybit" if leg == "long" else "Binance"), "action": action, "leg": leg})


def pnl_payload(attribution=None, funding="0.3", price_ref="1.0", opening_fee="0.1", closing_fee="0.1", slippage="4.0", net="-2.9", missing=()):
    payload = {"status": "COMPLETE", "reasons": [],
               "breakdown": {"legs": [], "total": {"funding": funding, "price_ref": price_ref, "price_actual": "-3.0", "opening_fee": opening_fee,
                                                   "closing_fee": closing_fee, "slippage": slippage, "other_cost": "0", "net": net},
                             "missing": list(missing), "status": "Complete", "other_cost_included": False}}
    if attribution is not None:
        payload["attribution"] = attribution
    return payload


def add_pair(db, pid, attribution=None, snaps=None, opens=STANDARD_OPENS, early_exit=False, pnls=None, finalized=True, **pnl_kw):
    """A finalized pair: entry snapshot(s), opening fills, closing fills, [early exit], FINALIZED and PnL event(s)."""
    for kw in snaps or [{}]:
        snapshot(db, pid, **kw)
    for leg, cid, qty, act in opens:
        fill(db, pid, leg, cid, qty, act)
    for leg, cid, act in (("long", "lc", "100.4"), ("short", "sc", "100.7")):
        fill(db, pid, leg, cid, "10", act, action="close")
    if early_exit:
        ev(db, pid, "LEG_EARLY_EXIT", {"leg": "short", "exchange": "Binance", "move_pct": "0.31", "threshold_pct": "0.3"})
    if finalized:
        ev(db, pid, "PAIR_TRANSITION", {"from": "CLOSING", "to": "FINALIZED"})
    for kind, kw in (pnls if pnls is not None else [("PAIR_PNL_COMPUTED", {"attribution": attribution})]):
        ev(db, pid, kind, pnl_payload(**{**pnl_kw, **kw}))


def make_db(path=":memory:", wal=False, **kw):
    db = sqlite3.connect(path)
    if wal:
        db.execute("pragma journal_mode=wal")
    schema(db)
    add_pair(db, "p-1", **kw)
    db.commit()
    return db


def closed_wal_db(directory, **kw):
    """A WAL-mode database as the app leaves it after a clean close: neither -wal nor -shm is left."""
    path = os.path.join(directory, "funding.db")
    db = make_db(path, wal=True, **kw)
    db.execute("pragma wal_checkpoint(TRUNCATE)")
    db.close()
    # the app's bundled SQLite deletes both files on a clean close; macOS's system SQLite (used by
    # Python here) keeps them, so remove them to get the state the app leaves behind
    for ext in ("-wal", "-shm"):
        if os.path.exists(path + ext):
            os.remove(path + ext)
    return path


def append_byte(path):
    with open(path, "ab") as f:
        f.write(b"\0")


def touch(path):
    with open(path, "wb"):
        pass


def patch_pnl(db, change):
    """Rewrite the stored PnL event's payload with `change(payload)`."""
    (payload,) = db.execute("select payload from events where event_type = 'PAIR_PNL_COMPUTED'").fetchone()
    p = json.loads(payload)
    change(p)
    db.execute("update events set payload = ? where event_type = 'PAIR_PNL_COMPUTED'", (json.dumps(p),))


def plain_db(directory):
    """An ordinary (rollback journal) database file."""
    path = os.path.join(directory, "funding.db")
    make_db(path).close()
    return path


def rows_of(db):
    return cr.load_pairs(db).rows


def line(text, start):
    return next(l for l in text.splitlines() if l.startswith(start))


def digest(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def three_pairs():
    """Three pairs of 1000 USDT per leg whose quantiles and back-test can be worked out by hand.
    total slippage % 0.2 / 0.4 / 0.8 ; drift % -0.1 / +0.2 / -0.3 ; net % -0.3 / +0.5 / -0.1 ; fees 0.06 % ;
    entry funding income (snapshot) 9.6 / 7.6 / 12.6 on 1000, fee 0.6, threshold 0.0021."""
    db = sqlite3.connect(":memory:")
    schema(db)
    data = [("A", "9.6", "1.0", "2.0", "-1.0", "0.6", "-3.0"),
            ("B", "7.6", "2.0", "4.0", "2.0", "7.6", "5.0"),
            ("C", "12.6", "4.0", "8.0", "-3.0", "10.6", "-1.0")]
    for pid, income, entry, slippage, price_ref, funding, net in data:
        add_pair(db, pid, attribution={"entry_slippage": entry, "notional_per_leg": "1000"}, snaps=[{"funding_income": income}],
                 slippage=slippage, price_ref=price_ref, opening_fee="0.3", closing_fee="0.3", funding=funding, net=net)
    return rows_of(db)


class Restore(unittest.TestCase):
    """An event without `attribution`: entry_slippage and notional_per_leg come from the raw events."""

    def test_standard_pair_values_and_breakdown_derivation(self):
        db = make_db()
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1")), (D("2.0"), D("1001"), None))
        row = rows_of(db)[0]
        self.assertTrue(row["restored"])
        self.assertEqual(row["notional_per_leg"], D("1001"))
        self.assertEqual(row["entry_slippage"]["usdt"], D("2.0"))
        self.assertEqual(row["slippage"]["usdt"], D("4.0"))
        self.assertEqual(row["exit_slippage"]["usdt"], D("2.0"))  # total slippage - entry
        self.assertEqual(row["drift"]["usdt"], D("1.0"))  # breakdown.total.price_ref
        self.assertEqual(row["fees"]["usdt"], D("0.2"))  # opening + closing
        self.assertEqual(row["funding"]["usdt"], D("0.3"))
        self.assertEqual(row["net"]["usdt"], D("-2.9"))
        self.assertAlmostEqual(float(row["entry_slippage"]["pct"]), 0.1998, places=4)  # usdt / notional per leg x 100
        identity = row["funding"]["usdt"] + row["drift"]["usdt"] - row["entry_slippage"]["usdt"] - row["exit_slippage"]["usdt"] - row["fees"]["usdt"]
        self.assertEqual(identity, row["net"]["usdt"])

    def test_two_open_orders_of_a_leg_are_summed_by_client_order_id_taking_each_orders_last_fill(self):
        # order c1 reports a partial fill (3 @ 100.4) and then its cumulative fill (6 @ 100.1); c2 is a second order
        opens = [("long", "c1", "3", "100.4"), ("long", "c1", "6", "100.1"), ("long", "c2", "4", "100.3"), ("short", "s1", "10", "100.1")]
        db = make_db(opens=opens)
        # long: dir -1 x (100 - act) x q = 0.1 x 6 + 0.3 x 4 = 1.8 ; short: 0.1 x 10 = 1.0
        # notional: long 6 x 100.1 + 4 x 100.3 = 1001.8, short 1001.0 -> mean 1001.4
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1")), (D("2.8"), D("1001.4"), None))

    def test_the_last_order_submit_snapshot_is_the_entry_reference(self):
        db = make_db(snaps=[{"long_ref": "99", "short_ref": "99"}, {"long_ref": "100", "short_ref": "100.2"}])
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1"))[0], D("2.0"))  # the first snapshot would give 0

    def test_a_missing_expected_price_is_unknown_and_mark_price_is_not_used(self):
        db = make_db(snaps=[{"long_ref": None}])  # the snapshot still has the long leg's mark_price 100.5
        entry, notional, why = cr.restore(cr.pair_events(db, "p-1"))
        self.assertIsNone(entry)
        self.assertIsNone(notional)  # the spec makes both null together
        self.assertIn("expected_price", why)

    def test_an_okx_leg_is_unknown(self):
        for name in ("OKX", "Okx"):  # the engine writes OKX
            with self.subTest(name):
                db = make_db(snaps=[{"short_ex": name}])
                entry, notional, why = cr.restore(cr.pair_events(db, "p-1"))
                self.assertEqual((entry, notional), (None, None))
                self.assertIn("OKX", why)

    def test_a_leg_without_an_open_fill_contributes_zero(self):
        db = make_db(opens=[("long", "lo", "10", "100.1")])  # the short leg never opened
        # entry only from the long leg (0.1 x 10 = 1.0) ; notional is that leg's alone (1001)
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1")), (D("1.0"), D("1001"), None))

    def test_an_okx_leg_that_never_opened_does_not_make_the_pair_unknown(self):
        db = make_db(opens=[("long", "lo", "10", "100.1")], snaps=[{"short_ex": "OKX"}])
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1"))[0], D("1.0"))

    def test_no_open_fill_at_all_is_unknown(self):
        entry, notional, why = cr.restore(cr.pair_events(make_db(opens=[]), "p-1"))
        self.assertEqual((entry, notional), (None, None))
        self.assertTrue(why)

    def test_an_order_that_never_filled_is_not_a_fill(self):
        db = make_db(opens=STANDARD_OPENS + [("short", "so2", "0", None)])  # e.g. a cancelled order: quantity 0, no price
        self.assertEqual(cr.restore(cr.pair_events(db, "p-1")), (D("2.0"), D("1001"), None))

    def test_a_price_that_is_not_a_number_skips_the_pair_instead_of_guessing(self):
        loaded = cr.load_pairs(make_db(opens=[("long", "lo", "10", "abc"), ("short", "so", "10", "100.1")]))
        self.assertEqual(loaded.rows, [])
        self.assertIn("avg_price", loaded.skipped[0][1])

    def test_a_fill_without_a_price_is_unknown(self):
        entry, notional, why = cr.restore(cr.pair_events(make_db(opens=[("long", "lo", "10", None), ("short", "so", "10", "100.1")]), "p-1"))
        self.assertEqual((entry, notional), (None, None))
        self.assertIn("avg_price", why)


class Attribution(unittest.TestCase):
    def test_the_latest_event_without_attribution_is_restored_not_taken_from_an_older_event(self):
        stale = {"entry_slippage": "99", "notional_per_leg": "5"}
        db = make_db(pnls=[("PAIR_PNL_COMPUTED", {"attribution": stale}), ("PAIR_PNL_RECOMPUTED", {})])
        row = rows_of(db)[0]
        self.assertTrue(row["restored"])
        self.assertEqual(row["entry_slippage"]["usdt"], D("2.0"))
        self.assertEqual(row["notional_per_leg"], D("1001"))
        self.assertIn("還原值", line(cr.render_report(cr.load_pairs(db)), "| p-1"))

    def test_the_latest_event_with_attribution_is_used_as_it_is(self):
        # attribution differs from what the raw events would give: the stored values win, the row is not "還原值"
        att = {"entry_slippage": "7", "notional_per_leg": "500"}
        db = make_db(pnls=[("PAIR_PNL_COMPUTED", {}), ("PAIR_PNL_RECOMPUTED", {"attribution": att})])
        row = rows_of(db)[0]
        self.assertFalse(row["restored"])
        self.assertEqual((row["entry_slippage"]["usdt"], row["notional_per_leg"]), (D("7"), D("500")))
        self.assertEqual(row["exit_slippage"]["usdt"], D("-3"))  # 4.0 - 7
        self.assertNotIn("還原值", line(cr.render_report(cr.load_pairs(db)), "| p-1"))

    def test_attribution_and_restored_agree(self):
        a = rows_of(make_db(attribution=STANDARD_ATTRIBUTION))[0]
        r = rows_of(make_db())[0]
        self.assertFalse(a["restored"])
        self.assertTrue(r["restored"])
        for k in ("entry_slippage", "exit_slippage", "slippage", "drift", "fees", "funding", "net"):
            self.assertEqual(a[k]["usdt"], r[k]["usdt"], k)
            self.assertEqual(a[k]["pct"], r[k]["pct"], k)
        self.assertEqual(a["notional_per_leg"], r["notional_per_leg"])

    def test_a_null_notional_skips_the_pair(self):
        loaded = cr.load_pairs(make_db(attribution={"entry_slippage": "2.0", "notional_per_leg": None}))
        self.assertEqual(loaded.rows, [])
        self.assertEqual([pid for pid, _ in loaded.skipped], ["p-1"])
        self.assertIn("notional_per_leg", loaded.skipped[0][1])

    def test_an_attribution_in_an_unrecognised_shape_skips_the_pair_with_a_reason(self):
        # the first design stored {"usdt", "pct"} objects: not the contract, and not something to guess from
        loaded = cr.load_pairs(make_db(attribution={"entry_slippage": {"usdt": "2.0", "pct": "0.2"}, "notional_per_leg": "1001"}))
        self.assertEqual(loaded.rows, [])
        self.assertIn("attribution.entry_slippage", loaded.skipped[0][1])

    def test_a_null_entry_slippage_makes_entry_and_exit_unknown_but_keeps_the_rest(self):
        row = rows_of(make_db(attribution={"entry_slippage": None, "notional_per_leg": "1001"}))[0]
        self.assertIsNone(row["entry_slippage"]["usdt"])
        self.assertIsNone(row["exit_slippage"]["usdt"])
        self.assertEqual(row["slippage"]["usdt"], D("4.0"))
        self.assertEqual(row["drift"]["usdt"], D("1.0"))

    def test_a_component_in_breakdown_missing_makes_only_its_items_unknown(self):
        cases = {"Slippage": ("slippage", "exit_slippage"), "PriceRef": ("drift",), "Funding": ("funding",),
                 "OpeningFee": ("fees",), "ClosingFee": ("fees",), "Net": ("net",)}
        everything = ("entry_slippage", "exit_slippage", "slippage", "drift", "fees", "funding", "net")
        for component, unknown in cases.items():
            with self.subTest(component):
                row = rows_of(make_db(attribution=STANDARD_ATTRIBUTION, missing=[component]))[0]
                for item in everything:
                    if item in unknown:
                        self.assertIsNone(row[item]["usdt"], item)
                        self.assertIsNone(row[item]["pct"], item)
                    else:
                        self.assertIsNotNone(row[item]["usdt"], item)
        self.assertEqual(rows_of(make_db(attribution=STANDARD_ATTRIBUTION, missing=["Slippage"]))[0]["entry_slippage"]["usdt"], D("2.0"))

    def test_missing_slippage_shows_a_dash_for_the_exit_not_a_zero(self):
        db = make_db(attribution=STANDARD_ATTRIBUTION, missing=["Net", "PriceRef", "Slippage"])  # a partial close
        cells = [c.strip() for c in line(cr.render_report(cr.load_pairs(db)), "| p-1").strip("|").split("|")]
        # pair | source | early exit | entry | drift | exit | fees | funding | net
        self.assertEqual(cells[3:9], ["(0.1998%)", "—", "—", "(0.0200%)", "+0.0300%", "—"])


class Completeness(unittest.TestCase):
    def test_skipped_pairs_are_listed_with_their_reason_and_the_counts_are_shown(self):
        db = sqlite3.connect(":memory:")
        schema(db)
        add_pair(db, "p-ok", attribution=STANDARD_ATTRIBUTION)
        add_pair(db, "p-nopnl", pnls=[])  # finalized but no PnL event
        add_pair(db, "p-okx", snaps=[{"short_ex": "OKX"}])  # no attribution, OKX leg: notional unknown
        add_pair(db, "p-bad", pnls=[("PAIR_PNL_COMPUTED", {})])
        db.execute("update events set payload = ? where pair_id = 'p-bad' and event_type = 'PAIR_PNL_COMPUTED'", (json.dumps({"status": "COMPLETE"}),))
        add_pair(db, "p-open", finalized=False)  # not finalized: not counted at all
        loaded = cr.load_pairs(db)
        self.assertEqual(loaded.finalized, 4)
        self.assertEqual([r["pid"] for r in loaded.rows], ["p-ok"])
        reasons = dict(loaded.skipped)
        self.assertEqual(sorted(reasons), ["p-bad", "p-nopnl", "p-okx"])
        self.assertIn("PnL", reasons["p-nopnl"])
        self.assertIn("OKX", reasons["p-okx"])
        self.assertIn("breakdown", reasons["p-bad"])
        text = cr.render_report(loaded)
        self.assertIn("FINALIZED 總數 / 實際使用筆數：4 / 1", text)
        for pid in ("p-nopnl", "p-okx", "p-bad"):
            self.assertIn(f"- {pid}：{reasons[pid]}", text)
        self.assertNotIn("p-open", text)

    def test_a_breakdown_without_one_of_its_values_is_skipped_with_the_name(self):
        db = make_db(attribution=STANDARD_ATTRIBUTION)
        patch_pnl(db, lambda p: p["breakdown"]["total"].pop("price_ref"))
        loaded = cr.load_pairs(db)
        self.assertEqual(loaded.rows, [])
        self.assertIn("price_ref", loaded.skipped[0][1])

    def test_a_payload_outside_the_contract_skips_the_pair_instead_of_crashing(self):
        cases = {"a zero notional": ({"entry_slippage": "2.0", "notional_per_leg": "0"}, "每腿名目"),
                 "a negative notional": ({"entry_slippage": "2.0", "notional_per_leg": "-5"}, "每腿名目"),
                 "an attribution that is not an object": ("2.0", "attribution")}
        for label, (att, reason) in cases.items():
            with self.subTest(label):
                loaded = cr.load_pairs(make_db(attribution=att))
                self.assertEqual(loaded.rows, [])
                self.assertIn(reason, loaded.skipped[0][1])
        db = make_db(attribution=STANDARD_ATTRIBUTION)
        patch_pnl(db, lambda p: p["breakdown"].update(missing="Net"))  # a string, not a list of component names
        loaded = cr.load_pairs(db)
        self.assertEqual(loaded.rows, [])
        self.assertIn("breakdown.missing", loaded.skipped[0][1])

    def test_a_programming_error_is_not_swallowed_as_a_skipped_pair(self):
        with mock.patch.object(cr, "build_row", side_effect=RuntimeError("bug")):
            with self.assertRaises(RuntimeError):
                cr.load_pairs(make_db(attribution=STANDARD_ATTRIBUTION))

    def test_without_skipped_pairs_the_list_says_none(self):
        text = cr.render_report(cr.load_pairs(make_db(attribution=STANDARD_ATTRIBUTION)))
        self.assertIn("FINALIZED 總數 / 實際使用筆數：1 / 1", text)
        self.assertIn("略過的配對（0 筆）", text)

    def test_only_the_newest_pnl_event_of_a_pair_counts(self):
        db = make_db(pnls=[("PAIR_PNL_COMPUTED", {"attribution": STANDARD_ATTRIBUTION, "funding": "9.9"}),
                           ("PAIR_PNL_RECOMPUTED", {"attribution": STANDARD_ATTRIBUTION})])
        self.assertEqual(rows_of(db)[0]["funding"]["usdt"], D("0.3"))

    def test_the_early_exit_is_marked(self):
        db = sqlite3.connect(":memory:")
        schema(db)
        add_pair(db, "p-early", attribution=STANDARD_ATTRIBUTION, early_exit=True)
        add_pair(db, "p-normal", attribution=STANDARD_ATTRIBUTION)
        loaded = cr.load_pairs(db)
        self.assertEqual({r["pid"]: r["early_exit"] for r in loaded.rows}, {"p-early": True, "p-normal": False})
        text = cr.render_report(loaded)
        self.assertIn("提前出場", text)
        self.assertEqual([c.strip() for c in line(text, "| p-early").strip("|").split("|")][2], "是")
        self.assertEqual([c.strip() for c in line(text, "| p-normal").strip("|").split("|")][2], "否")


class Statistics(unittest.TestCase):
    def test_shares_count_only_the_adverse_part_of_each_pair(self):
        db = sqlite3.connect(":memory:")
        schema(db)
        # notional 1000. pair 1: entry +0.1 %, exit -0.05 % (favourable), drift -0.2 % (adverse), fees 0.2 %
        add_pair(db, "p-1", attribution={"entry_slippage": "1.0", "notional_per_leg": "1000"}, slippage="0.5", price_ref="-2.0", opening_fee="1.0", closing_fee="1.0")
        # pair 2: entry -0.1 % (favourable), exit +0.3 %, drift +0.1 % (favourable), fees 0.2 %
        add_pair(db, "p-2", attribution={"entry_slippage": "-1.0", "notional_per_leg": "1000"}, slippage="2.0", price_ref="1.0", opening_fee="1.0", closing_fee="1.0")
        # pair 3 has an unknown item (a partial close): left out of the shares
        add_pair(db, "p-3", attribution={"entry_slippage": "1.0", "notional_per_leg": "1000"}, missing=["Slippage"])
        shares, used = cr.cost_shares(rows_of(db))
        self.assertEqual(used, 2)
        # adverse sums: entry 0.1, exit 0.3, drift 0.2, fees 0.4 -> total 1.0 (a signed sum would give 0 / 0.25 / -0.1 / 0.4)
        self.assertEqual(shares, {"進場滑價": D(10), "出場滑價": D(30), "不利漂移": D(20), "手續費": D(40)})
        text = cr.render_report(cr.load_pairs(db))
        self.assertIn("max(0, 進場滑價)", text)  # the table header says what is counted
        self.assertIn("max(0, −漂移)", text)

    def test_percentile_and_suggestions(self):
        self.assertEqual(cr.percentile([D(1), D(2), D(3), D(4), D(5)], 50), D(3))
        self.assertEqual(cr.percentile([D(0), D(10)], 75), D("7.5"))
        row = rows_of(make_db())[0]
        text = cr.render_report(cr.load_pairs(make_db()))
        # total slippage 4.0 / 1001 = 0.3996 % ; / 4 = 0.0999 ; adverse drift: the drift is favourable (+1.0) -> 0
        self.assertEqual(row["slippage"]["usdt"], D("4.0"))
        self.assertIn("est_slippage_pct", text)
        self.assertIn("0.0999", text)
        self.assertIn("safety_margin_pct", text)
        self.assertEqual(cr.suggest([row])[50]["safety"], D(0))

    def test_suggestions_from_three_pairs(self):
        s = cr.suggest(three_pairs())
        # total slippage % [0.2, 0.4, 0.8] -> p50 0.4, p75 0.6, p90 0.72 ; est = / 4
        self.assertEqual([s[p]["est"] for p in (50, 75, 90)], [D("0.1"), D("0.15"), D("0.18")])
        # adverse drift % [0, 0.1, 0.3] -> p50 0.1, p75 0.2, p90 0.26
        self.assertEqual([s[p]["safety"] for p in (50, 75, 90)], [D("0.1"), D("0.2"), D("0.26")])

    def test_a_negative_slippage_quantile_does_not_become_a_negative_setting(self):
        db = make_db(attribution={"entry_slippage": "-5", "notional_per_leg": "1001"}, slippage="-8.0")
        s = cr.suggest(rows_of(db))[50]
        self.assertEqual(s["est"], D(0))  # est_slippage_pct must be >= 0
        self.assertTrue(s["clamped"])
        self.assertIn("0.0000（分位數為負，以 0 計）", line(cr.render_report(cr.load_pairs(db)), "| est_slippage_pct"))
        self.assertNotIn("分位數為負，以 0 計", line(cr.render_report(cr.load_pairs(make_db())), "| est_slippage_pct"))  # a positive one is shown as it is

    def test_in_sample_backtest_numbers(self):
        rows = three_pairs()
        # edge % = funding income % - fee % - 4 x est - safety, at 1000 : A 0.96 - 0.06, B 0.76 - 0.06, C 1.26 - 0.06 before est and safety
        p50 = cr.backtest(rows, D("0.1"), D("0.1"))  # A 0.4, B 0.2, C 0.7 : all three still enter
        self.assertEqual((p50["enter"], p50["testable"], p50["unable"]), (3, 3, 0))
        self.assertEqual((p50["wins"], p50["losses"], p50["unknown_net"]), (1, 2, 0))  # nets % A -0.3, B +0.5, C -0.1
        self.assertEqual(p50["net_total"], D("0.1"))
        p75 = cr.backtest(rows, D("0.15"), D("0.2"))  # A 0.1, B -0.1, C 0.4 : A and C
        self.assertEqual((p75["enter"], p75["wins"], p75["losses"], p75["net_total"]), (2, 0, 2, D("-0.4")))
        p90 = cr.backtest(rows, D("0.18"), D("0.26"))  # A -0.08, B -0.28, C 0.22 : C only
        self.assertEqual((p90["enter"], p90["wins"], p90["losses"], p90["net_total"]), (1, 0, 1, D("-0.1")))
        actual = cr.outcome(rows)  # the settings the pairs were really entered with
        self.assertEqual((actual["wins"], actual["losses"], actual["net_total"]), (1, 2, D("0.1")))

    def test_the_report_marks_a_suggestion_that_is_still_negative_in_sample(self):
        text = cr.render_report(cr.Loaded(three_pairs(), [], 3))
        self.assertNotIn("樣本內仍為負", line(text, "| p50 "))
        self.assertIn("樣本內仍為負", line(text, "| p75 "))
        self.assertIn("樣本內仍為負", line(text, "| p90 "))
        actual = line(text, "| 實際進場時的設定")
        self.assertIn("+0.1000%", actual)
        self.assertNotIn("樣本內仍為負", actual)
        self.assertIn("3 / 3", line(text, "| p50 "))  # still entering / back-testable
        self.assertIn("1 / 2", line(text, "| p50 "))  # wins / losses

    def test_a_pair_exactly_at_its_threshold_still_enters(self):
        # (funding income 1.0 - fee 0.6) / 1000 x 100 = 0.04 % with est = safety = 0
        for threshold, enters in (("0.04", 1), ("0.0400001", 0)):
            with self.subTest(threshold):
                rows = rows_of(make_db(attribution=STANDARD_ATTRIBUTION, snaps=[{"threshold": threshold}]))
                self.assertEqual(cr.backtest(rows, D(0), D(0))["enter"], enters)

    def test_pairs_whose_snapshot_lacks_the_inputs_are_unable_to_back_test_and_counted(self):
        db = sqlite3.connect(":memory:")
        schema(db)
        add_pair(db, "p-1", attribution=STANDARD_ATTRIBUTION)
        add_pair(db, "p-2", attribution=STANDARD_ATTRIBUTION, snaps=[{"edge": False}])  # net_edge = {"unavailable": ...}
        add_pair(db, "p-3", attribution=STANDARD_ATTRIBUTION, snaps=[{"threshold": None}])  # no threshold_pct
        rows = rows_of(db)
        result = cr.backtest(rows, D(0), D(0))
        self.assertEqual((result["testable"], result["unable"]), (1, 2))
        text = cr.render_report(cr.load_pairs(db))
        self.assertIn("無法回測 2 筆", text)
        self.assertEqual(cr.outcome(rows)["losses"], 3)  # the actual-settings line still counts all of them

    def test_small_sample_warning_and_header(self):
        text = cr.render_report(cr.load_pairs(make_db()))
        self.assertIn("樣本不足，僅供參考", text)
        self.assertIn("n=1", text)
        self.assertIn("demo", text.splitlines()[0])
        self.assertIn("不會自動套用", text.splitlines()[0])

    def test_no_warning_at_30(self):
        row = rows_of(make_db())[0]
        text = cr.render_report(cr.Loaded([row] * 30, [], 30))
        self.assertNotIn("樣本不足", text)
        self.assertIn("n=30", text)
        self.assertIn("樣本不足，僅供參考", cr.render_report(cr.Loaded([row] * 29, [], 29)))

    def test_the_market_limits_are_stated_at_the_top(self):
        head = "\n".join(cr.render_report(cr.load_pairs(make_db())).splitlines()[:6])
        self.assertIn("正式環境", head)
        self.assertIn("mark", head)
        self.assertIn("demo 與正式盤的價差", head)
        self.assertIn("不可當作正式環境的滑價", head)

    def test_an_empty_sample_still_renders(self):
        text = cr.render_report(cr.Loaded([], [("p-1", "沒有 PnL 事件")], 1))
        self.assertIn("FINALIZED 總數 / 實際使用筆數：1 / 0", text)
        self.assertIn("- p-1：沒有 PnL 事件", text)


class Reading(unittest.TestCase):
    def test_read_only(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            make_db(path).close()
            before = digest(path)
            db = cr.connect(path)
            with self.assertRaises(sqlite3.OperationalError):
                db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (1,'X','p','{}')")
            db.close()
            with mock.patch.object(cr, "DB", path), redirect_stdout(io.StringIO()) as out:
                cr.main()
            self.assertIn("p-1", out.getvalue())
            self.assertNotIn("以快照方式讀取", out.getvalue())  # an ordinary read says nothing about snapshots
            self.assertEqual(before, digest(path))

    def test_a_directory_name_with_special_characters_is_still_read_only_and_creates_no_file(self):
        # '#' starts a URI fragment (everything after it, "?mode=ro" included, is dropped), '?' starts the query and
        # '%41' is a percent escape. Unencoded, SQLite opens the truncated path "<base>/a" read-write and CREATES it.
        with tempfile.TemporaryDirectory() as base:
            for label, make in (("rollback journal", plain_db), ("closed wal", closed_wal_db)):
                with self.subTest(label):
                    d = os.path.join(base, f"a#b %41 c?d {label}")
                    os.mkdir(d)
                    path = make(d)
                    tree, before = (sorted(os.listdir(base)), sorted(os.listdir(d))), digest(path)
                    db = cr.connect(path)
                    self.assertEqual(len(rows_of(db)), 1)
                    with self.assertRaises(sqlite3.OperationalError):
                        db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (1,'X','p','{}')")
                    cr.finish(db)
                    db.close()
                    # no -wal, -shm, -journal and no file at the truncated path appeared
                    self.assertEqual((sorted(os.listdir(base)), sorted(os.listdir(d))), tree)
                    self.assertEqual(digest(path), before)

    def test_a_closed_wal_database_reads_as_a_snapshot_and_says_so(self):
        # the app keeps funding.db in WAL mode; once it is closed the -wal / -shm files are gone and a
        # plain mode=ro open fails ("unable to open database file")
        with tempfile.TemporaryDirectory() as d:
            path = closed_wal_db(d)
            before = digest(path)
            db = cr.connect(path)
            self.assertTrue(db.immutable)
            self.assertEqual(len(rows_of(db)), 1)
            with self.assertRaises(sqlite3.OperationalError):
                db.execute("insert into events (ts_ms, event_type, pair_id, payload) values (1,'X','p','{}')")
            cr.finish(db)  # nothing changed meanwhile: no complaint
            db.close()
            self.assertEqual(digest(path), before)
            with mock.patch.object(cr, "DB", path), redirect_stdout(io.StringIO()) as out:
                cr.main()
            self.assertEqual(out.getvalue().splitlines()[0], "以快照方式讀取（App 未執行）")
            self.assertEqual(sorted(os.listdir(d)), ["funding.db"])

    def test_an_immutable_read_aborts_when_the_database_changes_meanwhile(self):
        changes = {"modification time": lambda p: os.utime(p, ns=(1, 1)),
                   "size": lambda p: append_byte(p),
                   "-wal appears": lambda p: touch(p + "-wal"),
                   "-shm appears": lambda p: touch(p + "-shm")}
        for label, change in changes.items():
            with self.subTest(label), tempfile.TemporaryDirectory() as d:
                path = closed_wal_db(d)
                real = cr.load_pairs

                def racing(db, real=real, path=path, change=change):
                    loaded = real(db)
                    change(path)  # the app starts (or something writes) while the report is reading
                    return loaded

                with mock.patch.object(cr, "DB", path), mock.patch.object(cr, "load_pairs", racing), redirect_stdout(io.StringIO()) as out:
                    with self.assertRaises(SystemExit) as stop:
                        cr.main()
                self.assertTrue(stop.exception.code)  # a non-zero exit with a message, not a report
                self.assertIn("重跑", str(stop.exception.code))
                self.assertEqual(out.getvalue(), "")

    def test_a_failed_mode_ro_open_with_a_wal_file_present_is_an_error_not_a_snapshot(self):
        with tempfile.TemporaryDirectory() as d:
            path = closed_wal_db(d)
            os.mkdir(path + "-wal")  # SQLite cannot open it ("unable to open database file") but it exists: the app may be running
            with self.assertRaises(sqlite3.OperationalError) as err:
                cr.connect(path)
            self.assertIn("unable to open database file", str(err.exception))

    def test_a_missing_database_file_is_an_error_not_a_snapshot(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(FileNotFoundError):
                cr.connect(os.path.join(d, "nope.db"))

    def test_other_errors_are_raised(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            with open(path, "wb") as f:
                f.write(b"this is not a database" * 100)
            with self.assertRaises(sqlite3.DatabaseError):
                cr.connect(path)

    def test_all_queries_run_in_one_read_transaction(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            app = make_db(path, wal=True)  # stays open, like the running app: -wal and -shm exist
            db = cr.connect(path)
            self.assertFalse(db.immutable)
            self.assertTrue(db.in_transaction)
            before = db.execute("select count(*) from events").fetchone()[0]
            add_pair(app, "p-2")
            app.commit()  # the app writes between two queries of the report
            self.assertEqual(db.execute("select count(*) from events").fetchone()[0], before)  # still the same snapshot
            cr.finish(db)
            self.assertFalse(db.in_transaction)
            self.assertGreater(db.execute("select count(*) from events").fetchone()[0], before)
            db.close()
            app.close()

    def test_no_usable_pair_prints_the_report_and_exits_with_an_error(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "funding.db")
            make_db(path, pnls=[]).close()  # finalized, no PnL event
            with mock.patch.object(cr, "DB", path), redirect_stdout(io.StringIO()) as out:
                with self.assertRaises(SystemExit) as stop:
                    cr.main()
            self.assertTrue(stop.exception.code)
            self.assertIn("- p-1：沒有 PnL 事件", out.getvalue())


if __name__ == "__main__":
    unittest.main()

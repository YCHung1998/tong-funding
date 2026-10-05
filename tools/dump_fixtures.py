#!/usr/bin/env python3
"""Exports parity fixtures from the Python reference implementation (read-only).

Source functions (mvp-python, pure, no I/O):
  quantity_precision: round_down_to_step / apply_lot_size / format_quantity / okx_base_qty_to_contracts
  pretrade_check:     evaluate_pretrade  (only the price-drift / margin / leverage checks)
  position_grouping:  group_positions_by_pair

Output: core/tests/fixtures/{quantity,pretrade,grouping}.json. Numbers are stored as strings so
the Rust side parses them as Decimal and never goes through floating point.

Deliberately NOT exported (no Python counterpart or not a pure function): Net Edge, funding
interval derivation, the Pair state machine, scheduler/pipeline integration behaviour.
Cases that sit on a float rounding boundary are excluded on purpose (Python's answer there is
float noise, not intended behaviour); Rust covers boundaries with hand-written tests.

Usage: python3 tools/dump_fixtures.py [--backend PATH] [--out DIR]
"""
import argparse, hashlib, json, os, random, subprocess, sys
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path

sys.dont_write_bytecode = True  # never create __pycache__ inside the source project
os.environ["PYTHONDONTWRITEBYTECODE"] = "1"

DEFAULT_BACKEND = "/Users/eason.hung/orca/workspaces/funding-analysis/mvp-python"
ROOT = Path(__file__).resolve().parent.parent


def plain(x: float) -> str:
    """Float -> plain decimal string without exponent or float noise beyond repr."""
    return format(Decimal(repr(x)), "f")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(backend: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(backend), *args], capture_output=True, text=True).stdout.strip()


def header(backend: Path, files: list[str], functions: list[str]) -> dict:
    return {
        "source": {
            "repo": str(backend),
            "git_commit": git(backend, "rev-parse", "HEAD"),
            "working_tree_dirty": bool(git(backend, "status", "--porcelain")),
            "files_sha256": {f: sha256(backend / f) for f in files},
            "functions": functions,
        },
        "exported_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


# ---------------------------------------------------------------- quantity
STEPS = ["0.001", "0.01", "0.1", "1", "10", "0.5", "0.0001"]


def quantity_cases(qp) -> list[dict]:
    rng = random.Random(20261005)
    fixed = [
        ("2.14589213", "0.1", "0.1"), ("2.3", "0.1", "0.1"), ("0.0004", "0.001", "0.001"),
        ("0.019933554", "0.001", "0.001"), ("1.0", "1", "1"), ("0.999999", "1", "1"),
        ("5", "0.5", "0.5"), ("0.0005", "0.0001", "0.0001"), ("123.456789", "0.01", "0.01"),
        ("0.07", "0.01", "0.01"), ("0.3", "0.1", "0.1"), ("1234.5678", "10", "10"), ("9.99", "10", "10"),
    ]
    triples = list(fixed)
    for _ in range(150):
        step = rng.choice(STEPS)
        qty = f"{rng.uniform(0.0001, 5000):.8f}".rstrip("0").rstrip(".")
        triples.append((qty, step, step))
    cases = []
    for qty, step, min_qty in triples:
        lot = {"step_size": float(step), "min_qty": float(min_qty)}
        adjusted = qp.apply_lot_size(float(qty), lot)
        case = {"input": {"qty": qty, "step_size": step, "min_qty": min_qty}}
        if adjusted == 0.0:
            case["expected"] = {"below_min": True}
        else:
            case["expected"] = {"below_min": False, "qty": plain(adjusted), "formatted": qp.format_quantity(adjusted, lot)}
        cases.append(case)
    return cases


def okx_cases(qp) -> list[dict]:
    rng = random.Random(77)
    fixed = [("0.021", "0.01", "1", "1"), ("0.0199", "0.01", "1", "1"), ("0.004", "0.01", "1", "1"),
             ("0.07", "0.01", "1", "1"), ("12.5", "1", "1", "1"), ("0.3", "0.1", "1", "1")]
    rows = list(fixed)
    for _ in range(60):
        ct = rng.choice(["0.01", "0.1", "1", "10", "0.001"])
        base = f"{rng.uniform(0.001, 500):.6f}".rstrip("0").rstrip(".")
        rows.append((base, ct, "1", "1"))
    cases = []
    for base, ct_val, lot_sz, min_sz in rows:
        contracts = qp.okx_base_qty_to_contracts(float(base), float(ct_val))
        lot = {"step_size": float(lot_sz), "min_qty": float(min_sz)}
        adjusted = qp.apply_lot_size(contracts, lot)
        case = {"input": {"base_qty": base, "ct_val": ct_val, "lot_sz": lot_sz, "min_sz": min_sz}}
        case["expected"] = {"below_min": True} if adjusted == 0.0 else {"below_min": False, "contracts": plain(adjusted)}
        cases.append(case)
    return cases


# ---------------------------------------------------------------- pretrade
def classify(reason: str) -> str:
    if "price drift" in reason:
        return "PriceDrift"
    if reason.startswith("Insufficient"):
        return "Margin"
    if reason.startswith("Leverage"):
        return "Leverage"
    raise ValueError(f"unclassified reason: {reason}")


def pretrade_cases(pc) -> list[dict]:
    rng = random.Random(4242)
    cases = []
    attempts = 0
    while len(cases) < 150 and attempts < 5000:
        attempts += 1
        long_base = round(rng.uniform(0.5, 90000), 2)
        short_base = round(rng.uniform(0.5, 90000), 2)
        max_drift = rng.choice(["0.05", "0.1", "0.5", "1"])
        use_pretrade_baseline = rng.random() < 0.7
        # latest prices: drift up to +-2% of baseline
        spread = 0.004 if rng.random() < 0.7 else 0.02   # mostly small drift so PASS and single failures occur
        long_latest = round(long_base * (1 + rng.uniform(-spread, spread)), 2)
        short_latest = round(short_base * (1 + rng.uniform(-spread, spread)), 2)
        if long_latest <= 0 or short_latest <= 0:
            continue
        margin_needed = rng.choice([100, 400, 400])
        avail_long = rng.choice([50, 399, 400, 401, 5000, 5000, 5000, 5000])
        avail_short = rng.choice([50, 399, 400, 401, 5000, 5000, 5000, 5000])
        leverage = rng.choice([1, 3, 5, 5, 5, 6])
        max_lev = rng.choice([5, 10, 10])
        # Skip float-boundary cases: drift within 1e-6 of the threshold, margin == need handled below.
        skip = False
        for base, latest in ((long_base, long_latest), (short_base, short_latest)):
            drift = abs(latest - base) / base * 100
            if abs(drift - float(max_drift)) < 1e-6:
                skip = True
        if skip:
            continue
        entry = {"margin_usdt": margin_needed, "leverage": leverage,
                 "long_exchange": "Binance", "short_exchange": "Bybit"}
        if use_pretrade_baseline:
            entry["pretrade_long_price"] = long_base
            entry["pretrade_short_price"] = short_base
            entry["long_price"] = round(long_base * 1.5, 2)   # stale scan price must be ignored
            entry["short_price"] = round(short_base * 1.5, 2)
        else:
            entry["long_price"] = long_base
            entry["short_price"] = short_base
        res = pc.evaluate_pretrade(entry, long_latest, short_latest, float(avail_long), float(avail_short),
                                   float(max_drift), float(max_lev))
        failed = sorted({classify(r) for r in res["reasons"]})
        assert res["pass"] == (not failed)
        cases.append({
            "input": {
                "baseline_source": "pretrade" if use_pretrade_baseline else "scan",
                "long_baseline": plain(long_base), "short_baseline": plain(short_base),
                "long_latest": plain(long_latest), "short_latest": plain(short_latest),
                "margin_needed": str(margin_needed),
                "available_margin_long": str(avail_long), "available_margin_short": str(avail_short),
                "leverage": str(leverage), "max_leverage": str(max_lev),
                "max_price_drift_pct": max_drift,
            },
            "expected": {"pass": res["pass"], "failed_checks": failed},
        })
    return cases


# ---------------------------------------------------------------- grouping
def grouping_cases(pg) -> list[dict]:
    rng = random.Random(909)
    exchanges = ["Binance", "Bybit", "OKX"]
    symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
    cases = []
    handmade = [
        ([("Binance", "BTCUSDT"), ("Bybit", "BTCUSDT")], [("Binance", "Bybit", "BTCUSDT")]),
        ([("Binance", "BTCUSDT"), ("Bybit", "BTCUSDT")], [("Binance", "Bybit", "BTCUSDT"), ("Binance", "Bybit", "BTCUSDT")]),
        ([("Binance", "BTCUSDT")], [("Binance", "Bybit", "BTCUSDT")]),
        ([], [("Binance", "Bybit", "BTCUSDT")]),
        ([("Binance", "BTCUSDT"), ("Bybit", "BTCUSDT")], []),
    ]
    scenarios = list(handmade)
    for _ in range(80):
        rows = [(rng.choice(exchanges), rng.choice(symbols)) for _ in range(rng.randint(0, 7))]
        entries = []
        for _ in range(rng.randint(0, 4)):
            a, b = rng.sample(exchanges, 2)
            entries.append((a, b, rng.choice(symbols)))
        scenarios.append((rows, entries))
    for rows_t, entries_t in scenarios:
        rows = [{"Exchange": e, "Symbol": s} for e, s in rows_t]
        entries = [{"long_exchange": a, "short_exchange": b, "symbol": s} for a, b, s in entries_t]
        out = pg.group_positions_by_pair(rows, entries)
        ident = {id(r): i for i, r in enumerate(rows)}
        entry_ident = {id(e): i for i, e in enumerate(entries)}
        grouped = [[entry_ident[id(g["entry"])], ident[id(g["rows"][0])], ident[id(g["rows"][1])]] for g in out["grouped"]]
        ungrouped = [ident[id(r)] for r in out["ungrouped"]]
        cases.append({
            "input": {"rows": [{"exchange": e, "symbol": s} for e, s in rows_t],
                      "entries": [{"long_exchange": a, "short_exchange": b, "symbol": s} for a, b, s in entries_t]},
            "expected": {"grouped": grouped, "ungrouped": ungrouped},
        })
    return cases


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--backend", default=DEFAULT_BACKEND)
    ap.add_argument("--out", default=str(ROOT / "core" / "tests" / "fixtures"))
    args = ap.parse_args()
    backend = Path(args.backend)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(backend))
    import quantity_precision as qp
    import pretrade_check as pc
    import position_grouping as pg

    def write(name: str, hdr: dict, cases: list[dict]) -> None:
        doc = {"header": hdr, "case_count": len(cases), "cases": cases}
        (out / name).write_text(json.dumps(doc, ensure_ascii=False, indent=1, sort_keys=True) + "\n")
        print(f"{name}: {len(cases)} cases")

    write("quantity.json",
          header(backend, ["quantity_precision.py"], ["apply_lot_size", "format_quantity"]),
          quantity_cases(qp))
    write("quantity_okx.json",
          header(backend, ["quantity_precision.py"], ["okx_base_qty_to_contracts", "apply_lot_size"]),
          okx_cases(qp))
    write("pretrade.json",
          header(backend, ["pretrade_check.py"], ["evaluate_pretrade"]),
          pretrade_cases(pc))
    write("grouping.json",
          header(backend, ["position_grouping.py"], ["group_positions_by_pair"]),
          grouping_cases(pg))
    return 0


if __name__ == "__main__":
    sys.exit(main())

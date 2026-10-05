//! Funding ledger fetching, PnL recording and reconciliation (change funding-pnl). Pure
//! computation lives in `tong_funding_core::pnl`; this module fetches (through injected ledger
//! sources), reads and writes the store.
//!
//! Timing values are design D9's provisional proposal (NOT verified; task 5.1 calibrates them).
#![allow(dead_code)]

pub mod fetch;
pub mod pnl_record;
pub mod reconcile;

/// First ledger fetch after an expected settlement (design D9, provisional).
pub const FETCH_DELAY_MS: i64 = 60_000;
/// Retry period while an expected settlement has no ledger entry (design D9, provisional).
pub const FETCH_RETRY_MS: i64 = 60_000;
/// How long after the closed confirmation the PnL waits for missing funding entries before it
/// is recorded as INCOMPLETE (design D9, provisional; user decision 2026-10-05: FINALIZED then
/// follows with INCOMPLETE and the missing items listed).
pub const PNL_RETRY_WINDOW_MS: i64 = 600_000;

/// Events of this change (Open Question 8: to be merged into the event schema document).
pub const PAIR_PNL_COMPUTED: &str = "PAIR_PNL_COMPUTED";
pub const PAIR_PNL_RECOMPUTED: &str = "PAIR_PNL_RECOMPUTED";
pub const PNL_RECONCILIATION: &str = "PNL_RECONCILIATION";
/// A completed fetch of a ledger window (what the PnL uses to tell "fetched, nothing there"
/// from "never fetched").
pub const FUNDING_LEDGER_FETCHED: &str = "FUNDING_LEDGER_FETCHED";
/// A failed ledger fetch (spec funding-history-fetch: `FETCH_ERROR`).
pub const FETCH_ERROR: &str = "FETCH_ERROR";

//! Trading engine (change: engine-simulation): one actor owns all mutable trading state; the UI
//! sends `Command`s and reads `Snapshot`s. Time comes from an injected clock only.
//!
//! Module ownership while built in parallel (each file has one owner; `command`, `ports`,
//! `timings` are the shared contract and change only by agreement):
//! - `command`, `ports`, `timings`: shared contract (task 1.1).
//! - `actor`, `transition`, `gate`: actor loop, land-then-act transitions, mode / kill switch gating
//!   (tasks 1.2, 1.3, 3.2).
//! - `sim`, `ids`, `intent`: `SimulatedExecutor` + executor factory, deterministic
//!   `client_order_id`, intent-first submission (tasks 3.1, 4.1).
//! - `schedule`, `node0`, `fill`: pure scheduling, Node 0 input assembly and fill / timeout
//!   decisions (tasks 2.1–2.3 logic).
//! - `recovery`: read-only restart reconciliation and its report for the startup gate (task 4.2).
//! - `alert`, `latency` (change exchange-demo-execution): alert reasons / notifier / once-per-entry
//!   bookkeeping, and `ORDER_LATENCY` events with their p50/p95/p99 aggregation.
#![allow(dead_code)]

pub mod actor;
pub mod alert;
pub mod command;
pub mod fill;
pub mod gate;
pub mod ids;
pub mod intent;
pub mod latency;
pub mod node0;
pub mod ports;
pub mod recovery;
pub mod schedule;
pub mod sim;
pub mod timings;
pub mod transition;

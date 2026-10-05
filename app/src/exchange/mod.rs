//! Exchange access (changes: exchange-readonly-adapters, later exchange-demo-execution).
//! Ownership while built in parallel: `public/**` = public REST adapters, `signed/**` and
//! `health/**` and `reqwest_transport.rs` = signed GET clients, feeds, clock sync, rate limiting.
#![allow(dead_code)]

pub mod error;
pub mod health;
pub mod public;
pub mod reqwest_transport;
pub mod signed;
pub mod transport;

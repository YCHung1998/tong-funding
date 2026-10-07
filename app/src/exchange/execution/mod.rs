//! Signed ORDER execution on the Binance and Bybit demo/testnet accounts (change:
//! exchange-demo-execution). The only module that may build POST / DELETE requests or name an
//! order-mutating fn (static checks in `exchange::static_checks`). Hosts are the compile-time
//! demo/testnet constants of `signed::endpoints`; no production host, no host override, no OKX.
//!
//! - `http`: order transport (method + demo host), real `reqwest` transport, test fake.
//! - `order`: validated order input (`ClientOrderId` required at the type level).
//! - `binance`, `bybit`: request building, signing, reply parsing.
//! - `classify`: accepted / rejected / rate limited / unknown.
//! - `executor`: `DemoExecutor` (the real `engine::ports::Executor`).
//! - `factory`: `DemoExecutorFactory` (the real `engine::ports::ExecutorFactory`).
//! - `account`: `DemoAccountView` (the real `engine::ports::AccountView`, signed GETs).
#![allow(dead_code)]

pub mod account;
pub mod binance;
pub mod bybit;
pub mod classify;
pub mod endpoints;
pub mod executor;
pub mod factory;
pub mod http;
pub mod okx;
pub mod order;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod executor_tests;
#[cfg(test)]
mod leverage_tests;
#[cfg(test)]
mod live_probe;
#[cfg(test)]
mod replay_tests;

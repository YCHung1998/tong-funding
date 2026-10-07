//! Signed GET clients (balances, positions, open orders) for Binance and Bybit demo/testnet.
//! Hosts are compile-time constants limited to demo/testnet. Owned by the signed-clients work.
#![allow(dead_code)]

pub mod binance;
pub mod bybit;
pub mod endpoints;
pub mod ledger;
pub mod models;
pub mod signing;

#[cfg(test)]
mod leverage_cap_tests;

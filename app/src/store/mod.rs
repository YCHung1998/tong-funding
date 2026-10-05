//! SQLite persistence, Keychain secrets and the legacy event import (change: store-sqlite).
//! Ownership while built in parallel: `db`, `events`, `state`, `scan_buffer` = database and
//! durable state; `secrets`, `legacy_import` = Keychain and importer. `schema` is shared and fixed.
#![allow(dead_code)]

pub mod db;
pub mod events;
pub mod import_cli;
pub mod legacy_import;
pub mod scan_buffer;
pub mod schema;
pub mod secrets;
pub mod state;

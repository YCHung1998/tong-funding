//! In-memory ring buffer for `SCAN_RUN` summaries. They are never written to `events`
//! (that table has no exceptions and is never purged); a restart empties the buffer.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// Design D4: keep the most recent 200 scan summaries.
pub const DEFAULT_CAPACITY: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct ScanRecord {
    pub ts_ms: i64,
    pub pair_id: Option<String>,
    pub payload: serde_json::Value,
}

pub struct ScanBuffer {
    capacity: usize,
    items: Mutex<VecDeque<ScanRecord>>,
}

impl Default for ScanBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl ScanBuffer {
    pub fn new(capacity: usize) -> Self {
        ScanBuffer { capacity, items: Mutex::new(VecDeque::new()) }
    }

    /// Append a record; when full the oldest one is dropped.
    pub fn push(&self, record: ScanRecord) {
        if self.capacity == 0 {
            return;
        }
        let mut items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        while items.len() >= self.capacity {
            items.pop_front();
        }
        items.push_back(record);
    }

    /// Oldest first.
    pub fn snapshot(&self) -> Vec<ScanRecord> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(n: i64) -> ScanRecord {
        ScanRecord { ts_ms: n, pair_id: None, payload: serde_json::json!({ "n": n }) }
    }

    #[test]
    fn default_capacity_is_200() {
        assert_eq!(DEFAULT_CAPACITY, 200);
        let b = ScanBuffer::default();
        for n in 0..250 {
            b.push(rec(n));
        }
        assert_eq!(b.len(), 200);
    }

    #[test]
    fn push_keeps_records_oldest_first() {
        let b = ScanBuffer::new(5);
        b.push(rec(1));
        b.push(rec(2));
        assert_eq!(b.snapshot(), vec![rec(1), rec(2)]);
    }

    #[test]
    fn overflow_drops_the_oldest_and_keeps_the_newest() {
        let b = ScanBuffer::new(3);
        for n in 1..=5 {
            b.push(rec(n));
        }
        assert_eq!(b.snapshot(), vec![rec(3), rec(4), rec(5)]);
    }
}

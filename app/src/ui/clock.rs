//! Dual clock formatting. Pure: takes a Unix timestamp, never reads the system clock,
//! so both readings always come from the same instant and are exactly 8 hours apart.

use chrono::{DateTime, FixedOffset, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockReading {
    pub utc_date: String,
    pub utc_time: String,
    pub taipei_date: String,
    pub taipei_time: String,
}

const TAIPEI_OFFSET_SECS: i32 = 8 * 3600;

pub fn read_clock(unix_secs: i64) -> ClockReading {
    let utc: DateTime<Utc> = DateTime::from_timestamp(unix_secs, 0).unwrap_or_default();
    let taipei = utc.with_timezone(&FixedOffset::east_opt(TAIPEI_OFFSET_SECS).expect("valid offset"));
    ClockReading {
        utc_date: utc.format("%Y-%m-%d").to_string(),
        utc_time: utc.format("%H:%M:%S").to_string(),
        taipei_date: taipei.format("%Y-%m-%d").to_string(),
        taipei_time: taipei.format("%H:%M:%S").to_string(),
    }
}

/// The one place the shell reads the wall clock (UI only; engine code must inject its clock).
pub fn now_unix_secs() -> i64 {
    Utc::now().timestamp()
}

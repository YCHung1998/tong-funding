//! Display formatting shared by the page view-models. Pure: no clock, no GPUI.
//! Negative numbers use U+2212 (−) like the specs; zero carries no sign.

use chrono::DateTime;
use rust_decimal::RoundingStrategy;
use tong_funding_core::types::Decimal;

pub const MINUS: char = '−';
/// Shown where a value does not exist (not the same as zero).
pub const DASH: &str = "—";

fn round(d: Decimal, dp: u32) -> Decimal {
    d.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero)
}

fn group_thousands(int_part: &str) -> String {
    let bytes = int_part.as_bytes();
    let mut out = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Absolute value with `dp` decimals and optional thousands separators.
fn magnitude(d: Decimal, dp: u32, thousands: bool) -> String {
    let r = round(d.abs(), dp);
    let s = format!("{:.*}", dp as usize, r);
    if !thousands {
        return s;
    }
    match s.split_once('.') {
        Some((i, f)) => format!("{}.{f}", group_thousands(i)),
        None => group_thousands(&s),
    }
}

fn with_sign(d: Decimal, dp: u32, body: String, plus: bool) -> String {
    let r = round(d, dp);
    if r.is_sign_negative() && !r.is_zero() {
        format!("{MINUS}{body}")
    } else if plus && !r.is_zero() {
        format!("+{body}")
    } else {
        body
    }
}

/// `22000` → `22,000.00` (dp = 2); negative → `−1,300.00`.
pub fn money(d: Decimal, dp: u32) -> String {
    with_sign(d, dp, magnitude(d, dp, true), false)
}

/// `4` → `+4.00`, `-4` → `−4.00`, `0` → `0.00`.
pub fn signed(d: Decimal, dp: u32) -> String {
    with_sign(d, dp, magnitude(d, dp, true), true)
}

/// Plain fixed decimals without separators (percentages, rates).
pub fn fixed(d: Decimal, dp: u32) -> String {
    with_sign(d, dp, magnitude(d, dp, false), false)
}

/// A funding rate given as a fraction, shown as a percentage number with 4 decimals
/// (`0.0005` → `0.0500`). Design D7: 4 decimals, not Figma's 3.
pub fn rate_pct(rate: Decimal) -> String {
    fixed(rate * Decimal::ONE_HUNDRED, 4)
}

/// Integral values without decimals (`2400` → `2,400`), others with 2.
pub fn compact(d: Decimal) -> String {
    if d.fract().is_zero() { money(d, 0) } else { money(d, 2) }
}

/// Position size: 6 decimals, unless a non-zero size would show as zero, then full precision.
pub fn size(qty: Decimal) -> String {
    let six = round(qty.abs(), 6);
    if six.is_zero() && !qty.is_zero() {
        qty.abs().normalize().to_string()
    } else {
        format!("{:.6}", six)
    }
}

/// `x / total * 100`; `None` when the denominator is not positive.
pub fn share_pct(part: Decimal, total: Decimal) -> Option<Decimal> {
    (total > Decimal::ZERO).then(|| part / total * Decimal::ONE_HUNDRED)
}

/// `2026-10-05 07:42:16.000` (UTC, milliseconds, with date).
pub fn utc_ms(ts_ms: i64) -> String {
    DateTime::from_timestamp_millis(ts_ms).map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()).unwrap_or_else(|| DASH.into())
}

/// `07:42:18` (UTC).
pub fn utc_hms(ts_ms: i64) -> String {
    DateTime::from_timestamp_millis(ts_ms).map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_else(|| DASH.into())
}

/// A non-negative duration as `HH:MM:SS` (hours may exceed 24).
pub fn hms(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// Whole seconds of a non-negative duration (rounded down).
pub fn secs(ms: i64) -> i64 {
    ms.max(0) / 1000
}

/// `BTCUSDT` → `BTC`; symbols without the suffix are returned as is.
pub fn base_coin(symbol: &str) -> &str {
    symbol.strip_suffix("USDT").filter(|b| !b.is_empty()).unwrap_or(symbol)
}

/// Funding interval label: `14400` → `4h`, `1800` → `30m`, unknown → `週期未知`.
pub fn interval_label(secs: Option<i64>) -> String {
    match secs.filter(|s| *s > 0) {
        Some(s) if s % 3600 == 0 => format!("{}h", s / 3600),
        Some(s) if s % 60 == 0 => format!("{}m", s / 60),
        Some(s) => format!("{s}s"),
        None => "週期未知".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn money_groups_thousands_and_rounds_half_away_from_zero() {
        assert_eq!(money(d("22000"), 2), "22,000.00");
        assert_eq!(money(d("1234567.005"), 2), "1,234,567.01");
        assert_eq!(money(d("-1300"), 2), "−1,300.00");
        assert_eq!(money(d("999"), 2), "999.00");
        assert_eq!(money(d("-0.001"), 2), "0.00", "rounds to zero: no sign");
    }

    #[test]
    fn signed_has_plus_minus_and_bare_zero() {
        assert_eq!(signed(d("4"), 2), "+4.00");
        assert_eq!(signed(d("-4"), 2), "−4.00");
        assert_eq!(signed(d("0"), 2), "0.00");
    }

    #[test]
    fn rate_pct_shows_percentage_with_four_decimals() {
        assert_eq!(rate_pct(d("0.0005")), "0.0500");
        assert_eq!(rate_pct(d("0.00005047")), "0.0050");
        assert_eq!(rate_pct(d("-0.0001")), "−0.0100");
    }

    #[test]
    fn size_uses_six_decimals_unless_that_would_hide_a_nonzero_value() {
        assert_eq!(size(d("0.02")), "0.020000");
        assert_eq!(size(d("-0.02")), "0.020000");
        assert_eq!(size(d("0.0000004")), "0.0000004");
        assert_eq!(size(d("0")), "0.000000");
    }

    #[test]
    fn compact_drops_decimals_for_integral_values() {
        assert_eq!(compact(d("2400")), "2,400");
        assert_eq!(compact(d("2400.5")), "2,400.50");
    }

    #[test]
    fn times_and_durations() {
        assert_eq!(utc_ms(1_791_185_736_000), "2026-10-05 07:35:36.000");
        assert_eq!(utc_hms(1_791_185_736_000), "07:35:36");
        assert_eq!(hms(4 * 3_600_000), "04:00:00");
        assert_eq!(hms(12 * 3_600_000 + 61_999), "12:01:01");
        assert_eq!(hms(-5), "00:00:00");
    }

    #[test]
    fn coins_and_interval_labels() {
        assert_eq!(base_coin("BTCUSDT"), "BTC");
        assert_eq!(base_coin("USDT"), "USDT");
        assert_eq!(interval_label(Some(14_400)), "4h");
        assert_eq!(interval_label(Some(28_800)), "8h");
        assert_eq!(interval_label(Some(3_600)), "1h");
        assert_eq!(interval_label(None), "週期未知");
    }
}

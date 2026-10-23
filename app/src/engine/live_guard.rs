//! live-guardrails: the pure pieces the engine enforces under the Live identity (spec
//! `live-guardrails`). Nothing here touches the store or an exchange; the actor wires it in.
//!
//! The rules (fail closed, the engine does not trust the screen):
//! - the confirmation sentence, the caps hash and the frozen caps used by `Command::Arm`;
//! - the per-leg notional check on `quantity x worst protection price`;
//! - the classification of an error text into an automatic lock cause (auth, HTTP 418 / 429);
//! - the defaults of a freshly created live database (`live_db_defaults`).

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use tong_funding_core::risk::{
    LIVE_CAP_CEILING_LEG_NOTIONAL_USDT, LIVE_CAP_CEILING_PAIRS_PER_ARMING, RiskConfig, TriggerMode,
};
use tong_funding_core::types::{Decimal, Exchange};

use super::preflight::PreflightReport;

/// The fixed sentence `Arm` requires (compared after `normalize_confirmation`).
pub const ARM_CONFIRMATION: &str = "我了解這是真錢 LIVE";
/// `Arm` succeeded: kill switch OFF, caps frozen (written with the kill switch change).
pub const LIVE_ARMED: &str = "LIVE_ARMED";
/// `Arm` was refused (reason only; never the typed sentence or an exchange response).
pub const LIVE_ARM_REFUSED: &str = "LIVE_ARM_REFUSED";
/// The engine locked itself (kill switch ON) for the written reason.
pub const LIVE_AUTO_LOCK: &str = "LIVE_AUTO_LOCK";
/// Live start: the kill switch was forced ON (every start, whatever it was before).
pub const LIVE_STARTUP_LOCK: &str = "LIVE_STARTUP_LOCK";
/// A preflight run finished (result only: pass / fail per item and the fixed reason text).
pub const LIVE_PREFLIGHT: &str = "LIVE_PREFLIGHT";
/// A leg of a pair was flattened by hand (`Command::FlattenLeg`).
pub const LEG_FLATTEN: &str = "LEG_FLATTEN";
/// A `FlattenLeg` that did not send an order (and why).
pub const LEG_FLATTEN_ABORTED: &str = "LEG_FLATTEN_ABORTED";

/// A preflight id stays usable this long after the engine issued it.
pub const PREFLIGHT_VALID_MS: i64 = 60_000;
/// Free disk space the preflight requires (100 MiB).
pub const MIN_FREE_DISK_BYTES: u64 = 100 * 1024 * 1024;
/// The two exchanges the Live identity trades (preflight and keys are checked on both).
pub const LIVE_EXCHANGES: [Exchange; 2] = [Exchange::Binance, Exchange::Bybit];
/// Protection buffer on the worst price (1 %).
pub const PROTECTION_BUFFER: Decimal = Decimal::from_parts(101, 0, 0, false, 2);
/// New live databases start with this contract-template notional per leg (USDT): below any
/// sensible cap, because the order check uses the worst protection price (1 % above the leg price).
pub const LIVE_DEFAULT_TEMPLATE_NOTIONAL_USDT: i64 = 9;

/// Comparison form of what the user typed. The standard library has no Unicode normalisation, so
/// this trims Unicode white space (including the ideographic space) only. NFC would change nothing
/// for the expected sentence (CJK unified ideographs and ASCII have no canonical decomposition);
/// it would only differ for CJK compatibility ideographs, which are refused here (fail closed).
pub fn normalize_confirmation(input: &str) -> &str {
    input.trim()
}

pub fn confirmation_matches(input: &str) -> bool {
    normalize_confirmation(input) == ARM_CONFIRMATION
}

/// The caps in force at arming (copied from the stored settings; memory only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenCaps {
    pub leg_notional_usdt: Decimal,
    pub pairs_per_arming: u32,
    pub max_concurrent_pairs: u32,
    pub max_leverage: Decimal,
}

impl FrozenCaps {
    /// `Err` (names the field) when a live cap is unset or above its built-in ceiling.
    pub fn from_risk(risk: &RiskConfig) -> Result<FrozenCaps, String> {
        let leg = risk.live_max_leg_notional_usdt.ok_or("live_max_leg_notional_usdt is not set")?;
        let pairs = risk.live_max_pairs_per_arming.ok_or("live_max_pairs_per_arming is not set")?;
        if leg <= Decimal::ZERO || leg > LIVE_CAP_CEILING_LEG_NOTIONAL_USDT {
            return Err("live_max_leg_notional_usdt is outside (0, ceiling]".into());
        }
        if pairs < 1 || pairs > LIVE_CAP_CEILING_PAIRS_PER_ARMING {
            return Err("live_max_pairs_per_arming is outside 1..=ceiling".into());
        }
        Ok(FrozenCaps {
            leg_notional_usdt: leg,
            pairs_per_arming: pairs,
            max_concurrent_pairs: risk.max_concurrent_pairs,
            max_leverage: risk.max_leverage,
        })
    }

    /// The first field of `new` that is higher than the frozen value (a removed live cap counts as
    /// higher); `None` when `new` only keeps or lowers every cap.
    pub fn first_raised_by(&self, new: &RiskConfig) -> Option<&'static str> {
        if new.live_max_leg_notional_usdt.is_none_or(|v| v > self.leg_notional_usdt) {
            return Some("live_max_leg_notional_usdt");
        }
        if new.live_max_pairs_per_arming.is_none_or(|v| v > self.pairs_per_arming) {
            return Some("live_max_pairs_per_arming");
        }
        if new.max_concurrent_pairs > self.max_concurrent_pairs {
            return Some("max_concurrent_pairs");
        }
        if new.max_leverage > self.max_leverage {
            return Some("max_leverage");
        }
        None
    }
}

/// Hash of the four caps `Arm` is bound to (hex SHA-256). `None` while a live cap is unset.
pub fn caps_hash(risk: &RiskConfig) -> Option<String> {
    let leg = risk.live_max_leg_notional_usdt?;
    let pairs = risk.live_max_pairs_per_arming?;
    let text = format!(
        "caps-v1|leg={}|pairs={}|concurrent={}|leverage={}",
        leg.normalize(),
        pairs,
        risk.max_concurrent_pairs,
        risk.max_leverage.normalize()
    );
    Some(hex::encode(Sha256::digest(text.as_bytes())))
}

/// The per-leg cap in force: the stored cap (never above the built-in ceiling), lowered to the
/// frozen value while armed. `Err` = unknown, callers refuse.
pub fn effective_leg_cap(stored: &RiskConfig, frozen: Option<&FrozenCaps>) -> Result<Decimal, String> {
    let stored_cap = stored.live_max_leg_notional_usdt.ok_or("live_max_leg_notional_usdt is not set")?;
    let mut cap = stored_cap.min(LIVE_CAP_CEILING_LEG_NOTIONAL_USDT);
    if let Some(f) = frozen {
        cap = cap.min(f.leg_notional_usdt);
    }
    Ok(cap)
}

/// The pairs-per-arming cap in force (same rule as [`effective_leg_cap`]).
pub fn effective_pairs_cap(stored: &RiskConfig, frozen: Option<&FrozenCaps>) -> Result<u32, String> {
    let stored_cap = stored.live_max_pairs_per_arming.ok_or("live_max_pairs_per_arming is not set")?;
    let mut cap = stored_cap.min(LIVE_CAP_CEILING_PAIRS_PER_ARMING);
    if let Some(f) = frozen {
        cap = cap.min(f.pairs_per_arming);
    }
    Ok(cap)
}

/// Worst price an opening order can fill at: the larger of the leg's base price and its latest
/// prices, plus the protection buffer.
pub fn worst_protection_price(candidates: &[Decimal]) -> Option<Decimal> {
    candidates.iter().copied().filter(|p| *p > Decimal::ZERO).max().map(|p| p * PROTECTION_BUFFER)
}

/// `Err` when `base_quantity x worst_price` is above `cap` (or cannot be computed).
pub fn check_leg_notional(cap: Decimal, base_quantity: Decimal, worst_price: Option<Decimal>) -> Result<(), String> {
    let Some(worst) = worst_price else {
        return Err("no usable price to bound the order notional (fail closed)".into());
    };
    let notional = base_quantity * worst;
    if notional > cap {
        Err(format!(
            "leg notional {} USDT (quantity {} x worst protection price {}) is above the live cap {} USDT",
            notional.round_dp(4).normalize(),
            base_quantity.normalize(),
            worst.round_dp(6).normalize(),
            cap.normalize()
        ))
    } else {
        Ok(())
    }
}

/// Why an error text locks the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCause {
    /// HTTP 401 / 403 or an exchange answer about keys, signature, permission or IP.
    Auth,
    /// HTTP 418 / 429 or an exchange rate-limit answer.
    RateLimited,
}

impl LockCause {
    pub fn as_str(self) -> &'static str {
        match self {
            LockCause::Auth => "authentication error",
            LockCause::RateLimited => "rate limited (HTTP 418/429)",
        }
    }
}

fn has_http_status(lower: &str, code: u16) -> bool {
    let code = code.to_string();
    ["http status ", "http ", "status ", "status code ", "http/1.1 ", "http/2 "].iter().any(|prefix| {
        let needle = format!("{prefix}{code}");
        lower.match_indices(&needle).any(|(at, _)| !lower[at + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
    })
}

fn has_code(lower: &str, exchange: &str, code: &str) -> bool {
    let needle = format!("{exchange} {code}");
    lower.match_indices(&needle).any(|(at, _)| !lower[at + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Conservative: it is better to lock on a doubtful text than to keep trading (design decision 7;
/// the exact exchange codes are to be completed after the phase-0 read-only run). Rate limits are
/// tested first so a text that is both is reported as the limit.
pub fn classify_error(text: &str) -> Option<LockCause> {
    let lower = text.to_lowercase();
    let rate = has_http_status(&lower, 418)
        || has_http_status(&lower, 429)
        || lower.contains("rate limited")
        || lower.contains("too many requests")
        || has_code(&lower, "binance", "-1003")
        || has_code(&lower, "bybit", "10006")
        || has_code(&lower, "bybit", "10018");
    if rate {
        return Some(LockCause::RateLimited);
    }
    let auth = has_http_status(&lower, 401)
        || has_http_status(&lower, 403)
        || ["-2014", "-2015", "-2008", "-1022"].iter().any(|c| has_code(&lower, "binance", c))
        || ["10003", "10004", "10005", "10007", "10010"].iter().any(|c| has_code(&lower, "bybit", c))
        || [
            "invalid api",
            "api-key",
            "api key",
            "apikey",
            "signature",
            "unauthorized",
            "unauthorised",
            "forbidden",
            "permission denied",
            "ip not allowed",
            "unmatched ip",
            "authentication failed",
            "invalid key",
        ]
        .iter()
        .any(|k| lower.contains(k));
    auth.then_some(LockCause::Auth)
}

/// What a freshly created live database starts with (`live_db_defaults`): MANUAL trigger, no early
/// exit, a loss breaker of 5 USDT or 2 losses, a contract template below any cap. The live caps stay unset on purpose (no default, so
/// arming is impossible until the user fills them in).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveDbDefaults {
    pub trigger_mode: TriggerMode,
    pub risk: RiskConfig,
    pub template_notional_usdt: Decimal,
    pub template_leverage: Decimal,
}

pub fn live_db_defaults() -> LiveDbDefaults {
    let risk = RiskConfig {
        trigger_mode: TriggerMode::Manual,
        early_exit_min_move_pct: Decimal::ZERO,
        // loss-circuit-breaker: real money starts with a stricter breaker (5 USDT or 2 losses in a row).
        loss_breaker_max_loss_usdt: Decimal::from(5),
        loss_breaker_max_consecutive_losses: 2,
        ..RiskConfig::default()
    };
    LiveDbDefaults {
        trigger_mode: TriggerMode::Manual,
        template_leverage: risk.max_leverage,
        risk,
        template_notional_usdt: Decimal::from(LIVE_DEFAULT_TEMPLATE_NOTIONAL_USDT),
    }
}

/// Engine-side Live state (memory only: a restart starts locked and unarmed).
#[derive(Debug, Default)]
pub struct LiveGuard {
    /// Latest preflight result (usable by `Arm` only when it carries an id and is fresh).
    pub preflight: Option<PreflightReport>,
    pub preflight_running: bool,
    /// `Some` = armed: the caps copied at `Arm`.
    pub frozen: Option<FrozenCaps>,
    /// Pairs that entered ORDER_SUBMIT since the last `Arm`.
    pub pairs_entered: u32,
    /// Pairs counted in `pairs_entered` whose opening orders have not been handed to the executor yet.
    pub unsent: BTreeSet<String>,
    /// (pair, alert state name) already answered with an automatic lock.
    pub locked_alerts: BTreeSet<(String, &'static str)>,
    pub last_lock_reason: Option<String>,
}

/// Read-only copy for the UI (`EngineHandle::live`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveStatus {
    pub is_live: bool,
    pub armed: bool,
    pub frozen: Option<FrozenCaps>,
    pub pairs_entered: u32,
    pub preflight_running: bool,
    pub preflight: Option<PreflightReport>,
    pub last_lock_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn risk(leg: &str, pairs: u32) -> RiskConfig {
        RiskConfig { live_max_leg_notional_usdt: Some(d(leg)), live_max_pairs_per_arming: Some(pairs), ..RiskConfig::default() }
    }

    #[test]
    fn confirmation_is_compared_exactly_after_trimming_whitespace() {
        assert!(confirmation_matches("我了解這是真錢 LIVE"));
        assert!(confirmation_matches("  我了解這是真錢 LIVE \n"));
        assert!(confirmation_matches("\u{3000}我了解這是真錢 LIVE\u{3000}"), "ideographic space is trimmed");
        for bad in ["", "我了解這是真錢 live", "我了解這是真錢LIVE", "我了解這是真钱 LIVE", "我了解這是真錢  LIVE", "LIVE", "我了解這是真錢 LIVE!"] {
            assert!(!confirmation_matches(bad), "{bad:?}");
        }
    }

    #[test]
    fn protection_buffer_is_one_percent() {
        assert_eq!(PROTECTION_BUFFER, d("1.01"));
        assert_eq!(worst_protection_price(&[d("100"), d("100.5")]), Some(d("101.505")));
        assert_eq!(worst_protection_price(&[d("0"), d("-1")]), None);
        assert_eq!(worst_protection_price(&[]), None);
    }

    #[test]
    fn leg_notional_check_uses_quantity_times_worst_price() {
        let worst = worst_protection_price(&[d("100")]);
        // 0.099 x 101 = 9.999 <= 10
        assert_eq!(check_leg_notional(d("10"), d("0.099"), worst), Ok(()));
        // exactly equal passes: 0.1 x 100 (no buffer)
        assert_eq!(check_leg_notional(d("10"), d("0.1"), Some(d("100"))), Ok(()));
        // 0.1 x 101 = 10.1 > 10: the buffer alone can push an order over the cap
        let err = check_leg_notional(d("10"), d("0.1"), worst).unwrap_err();
        assert!(err.contains("10.1") && err.contains("cap 10"), "{err}");
        assert!(check_leg_notional(d("10"), d("1"), None).is_err(), "no price: fail closed");
    }

    #[test]
    fn effective_caps_take_the_lowest_of_stored_ceiling_and_frozen() {
        let r = risk("10", 2);
        let frozen = FrozenCaps::from_risk(&r).unwrap();
        assert_eq!(frozen.leg_notional_usdt, d("10"));
        assert_eq!(effective_leg_cap(&r, None), Ok(d("10")));
        assert_eq!(effective_leg_cap(&r, Some(&frozen)), Ok(d("10")));
        // lowered after arming: the lower value takes effect at once
        assert_eq!(effective_leg_cap(&risk("4", 2), Some(&frozen)), Ok(d("4")));
        // raised after arming (should be refused at save, but the engine still holds the frozen value)
        let raised = RiskConfig { live_max_leg_notional_usdt: Some(d("20")), ..r.clone() };
        assert_eq!(effective_leg_cap(&raised, Some(&frozen)), Ok(d("10")));
        // a stored value above the ceiling (tampered database) is cut to the ceiling
        let tampered = RiskConfig { live_max_leg_notional_usdt: Some(d("5000")), ..r.clone() };
        assert_eq!(effective_leg_cap(&tampered, None), Ok(d("100")));
        assert_eq!(effective_pairs_cap(&RiskConfig { live_max_pairs_per_arming: Some(99), ..r.clone() }, None), Ok(5));
        assert_eq!(effective_pairs_cap(&risk("10", 1), Some(&FrozenCaps { pairs_per_arming: 2, ..frozen.clone() })), Ok(1));
        assert!(effective_leg_cap(&RiskConfig::default(), None).is_err(), "unset: unknown, refuse");
        assert!(effective_pairs_cap(&RiskConfig::default(), None).is_err());
    }

    #[test]
    fn frozen_caps_need_both_caps_inside_the_ceilings() {
        assert!(FrozenCaps::from_risk(&RiskConfig::default()).is_err());
        assert!(FrozenCaps::from_risk(&risk("101", 2)).is_err());
        assert!(FrozenCaps::from_risk(&risk("10", 6)).is_err());
        assert!(FrozenCaps::from_risk(&risk("10", 0)).is_err());
        assert!(FrozenCaps::from_risk(&risk("100", 5)).is_ok());
    }

    #[test]
    fn raising_any_frozen_cap_is_detected_and_lowering_is_not() {
        let r = risk("10", 2);
        let f = FrozenCaps::from_risk(&r).unwrap();
        assert_eq!(f.first_raised_by(&r), None);
        assert_eq!(f.first_raised_by(&risk("9", 1)), None);
        assert_eq!(f.first_raised_by(&risk("10.01", 2)), Some("live_max_leg_notional_usdt"));
        assert_eq!(f.first_raised_by(&risk("10", 3)), Some("live_max_pairs_per_arming"));
        assert_eq!(f.first_raised_by(&RiskConfig { max_concurrent_pairs: r.max_concurrent_pairs + 1, ..r.clone() }), Some("max_concurrent_pairs"));
        assert_eq!(f.first_raised_by(&RiskConfig { max_leverage: r.max_leverage + d("1"), ..r.clone() }), Some("max_leverage"));
        assert_eq!(f.first_raised_by(&RiskConfig::default()), Some("live_max_leg_notional_usdt"), "removing a cap is raising it");
    }

    #[test]
    fn caps_hash_binds_exactly_the_four_caps() {
        let r = risk("10", 2);
        let h = caps_hash(&r).unwrap();
        assert_eq!(h.len(), 64);
        assert_eq!(caps_hash(&risk("10.0", 2)), Some(h.clone()), "10 and 10.0 are the same cap");
        assert_ne!(caps_hash(&risk("11", 2)), Some(h.clone()));
        assert_ne!(caps_hash(&risk("10", 3)), Some(h.clone()));
        assert_ne!(caps_hash(&RiskConfig { max_concurrent_pairs: 9, ..r.clone() }), Some(h.clone()));
        assert_ne!(caps_hash(&RiskConfig { max_leverage: d("7"), ..r.clone() }), Some(h.clone()));
        // anything else does not change it
        assert_eq!(caps_hash(&RiskConfig { stale_data_threshold_ms: 1, ..r.clone() }), Some(h));
        assert_eq!(caps_hash(&RiskConfig::default()), None);
    }

    #[test]
    fn rate_limit_and_auth_texts_are_recognised_and_ordinary_failures_are_not() {
        use LockCause::*;
        for t in [
            "HTTP status 418",
            "HTTP 429 without an exchange error code",
            "rate limited (retry after Some(1000) ms)",
            "binance -1003: Too many requests (status unknown)",
            "bybit 10006: Too many visits",
            "status 429",
        ] {
            assert_eq!(classify_error(t), Some(RateLimited), "{t}");
        }
        for t in [
            "HTTP status 401",
            "HTTP 403",
            "binance -2015: Invalid API-key, IP, or permissions for action (status unknown)",
            "binance -1022: Signature for this request is not valid",
            "bybit 10003: API key is invalid",
            "bybit 10010: Unmatched IP",
            "Unauthorized",
            "no API key found for Binance",
        ] {
            assert_eq!(classify_error(t), Some(Auth), "{t}");
        }
        for t in [
            "request timed out",
            "network error: connection reset",
            "HTTP status 500",
            "HTTP status 4018",
            "binance -2010: Account has insufficient balance",
            "bybit 110007: insufficient available balance",
            "order 4290 not found",
            "",
        ] {
            assert_eq!(classify_error(t), None, "{t}");
        }
    }

    #[test]
    fn a_new_live_database_starts_manual_without_early_exit_and_without_caps() {
        let d = live_db_defaults();
        assert_eq!(d.trigger_mode, TriggerMode::Manual);
        assert_eq!(d.risk.trigger_mode, TriggerMode::Manual);
        assert_eq!(d.risk.early_exit_min_move_pct, Decimal::ZERO);
        assert_eq!(d.risk.live_max_leg_notional_usdt, None, "caps stay unset: arming impossible until filled in");
        assert_eq!(d.risk.live_max_pairs_per_arming, None);
        assert!(d.template_notional_usdt > Decimal::ZERO && d.template_notional_usdt <= LIVE_CAP_CEILING_LEG_NOTIONAL_USDT);
        assert!(d.risk.validate().is_ok());
    }

    #[test]
    fn a_new_live_database_starts_with_a_breaker_of_5_usdt_or_2_losses() {
        // loss-circuit-breaker 1.2: stricter than the Demo default of 15 / 3
        let d = live_db_defaults();
        assert_eq!(d.risk.loss_breaker_max_loss_usdt, Decimal::from(5));
        assert_eq!(d.risk.loss_breaker_max_consecutive_losses, 2);
        assert!(d.risk.validate().is_ok());
        assert_eq!(RiskConfig::default().loss_breaker_max_loss_usdt, Decimal::from(15), "the Demo default is untouched");
    }
}

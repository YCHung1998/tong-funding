//! live-guardrails 3.5: a freshly created LIVE database starts with the safe defaults of
//! `engine::live_guard::live_db_defaults` (MANUAL trigger, no early exit, a contract template of a
//! few USDT). "Fresh" = neither the `risk` nor the `contract_template` key was ever written. The
//! live caps stay unset on purpose: arming is impossible until the user sets them.
//! A demo database is never touched.

use serde_json::json;

use crate::engine::actor::CONFIG_CONTRACT_TEMPLATE;
use crate::engine::live_guard::live_db_defaults;
use crate::store::config_cli::KEY_RISK;
use crate::store::db::Db;
use crate::store::state::ConfigChange;

/// Writes the live defaults into a fresh live database; `Ok(true)` when it did, `Ok(false)` when
/// the database is not live or already has settings.
pub fn apply_if_fresh(db: &Db) -> Result<bool, String> {
    if !db.identity().is_live() {
        return Ok(false);
    }
    let read = |key: &str| db.config_get(key).map_err(|e| format!("cannot read {key}: {e}"));
    if read(KEY_RISK)?.is_some() || read(CONFIG_CONTRACT_TEMPLATE)?.is_some() {
        return Ok(false);
    }
    let d = live_db_defaults();
    let risk = serde_json::to_value(&d.risk).map_err(|e| format!("cannot encode live defaults: {e}"))?;
    let template = json!({ "notional_usdt": d.template_notional_usdt.to_string(), "leverage": d.template_leverage.to_string() });
    db.config_set_many(&[
        ConfigChange { key: KEY_RISK.into(), value: risk, expected_version: None },
        ConfigChange { key: CONFIG_CONTRACT_TEMPLATE.into(), value: template, expected_version: None },
    ])
    .map(|_| true)
    .map_err(|e| format!("cannot write live defaults: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::actor::load_risk_config;
    use crate::identity::ProcessIdentity;
    use crate::store::db::test_support::{clock, tempdir};
    use tong_funding_core::risk::TriggerMode;

    fn open(identity: ProcessIdentity, name: &str) -> (tempfile::TempDir, Db) {
        let dir = tempdir();
        let (c, _) = clock(1_000_000);
        let db = Db::open_for(identity, &dir.path().join(name), c);
        assert!(!db.is_halted());
        (dir, db)
    }

    #[test]
    fn a_fresh_live_database_gets_manual_trigger_no_early_exit_and_a_small_template_and_no_caps() {
        let (_d, db) = open(ProcessIdentity::Live, "funding-live.db");
        assert_eq!(apply_if_fresh(&db), Ok(true));
        let (risk, _) = load_risk_config(&db).unwrap();
        assert_eq!(risk.trigger_mode, TriggerMode::Manual);
        assert!(risk.early_exit_min_move_pct.is_zero());
        assert_eq!((risk.live_max_leg_notional_usdt, risk.live_max_pairs_per_arming), (None, None), "caps stay unset: no default arming");
        assert_eq!((risk.loss_breaker_max_loss_usdt, risk.loss_breaker_max_consecutive_losses), (rust_decimal::Decimal::from(5), 2), "loss-circuit-breaker: a stricter breaker for real money");
        let t = crate::ui::bridge::ContractTemplate::from_json(&db.config_get(CONFIG_CONTRACT_TEMPLATE).unwrap().unwrap().value).unwrap();
        assert_eq!(t.notional_usdt, live_db_defaults().template_notional_usdt);
        // idempotent: a second start changes nothing
        let v = db.config_get(KEY_RISK).unwrap().unwrap().version;
        assert_eq!(apply_if_fresh(&db), Ok(false));
        assert_eq!(db.config_get(KEY_RISK).unwrap().unwrap().version, v);
    }

    #[test]
    fn existing_live_settings_and_any_demo_database_are_left_alone() {
        let (_d, db) = open(ProcessIdentity::Live, "funding-live.db");
        db.config_set(KEY_RISK, &json!({"trigger_mode":"AUTO"}), None).unwrap();
        assert_eq!(apply_if_fresh(&db), Ok(false));
        assert!(db.config_get(CONFIG_CONTRACT_TEMPLATE).unwrap().is_none(), "nothing was added to a configured database");
        let (_d2, demo) = open(ProcessIdentity::Demo, "funding.db");
        assert_eq!(apply_if_fresh(&demo), Ok(false));
        assert!(demo.config_get(KEY_RISK).unwrap().is_none());
    }
}

//! Tests for ui-zoom spec (changes/ui-font-zoom/specs/ui-zoom/spec.md).
use serde_json::json;

use super::*;

#[test]
fn default_is_100() {
    assert_eq!(ZoomPct::default().get(), 100);
    assert_eq!(ZoomPct::reset().get(), 100);
}

#[test]
fn step_in_adds_ten() {
    assert_eq!(ZoomPct::default().step_in().get(), 110);
}

#[test]
fn step_out_subtracts_ten() {
    assert_eq!(ZoomPct::default().step_out().get(), 90);
}

#[test]
fn upper_bound_holds_at_200() {
    assert_eq!(ZoomPct::new(200).step_in().get(), 200);
}

#[test]
fn lower_bound_holds_at_70() {
    assert_eq!(ZoomPct::new(70).step_out().get(), 70);
}

#[test]
fn reset_from_70_is_100() {
    assert_eq!(ZoomPct::new(70).get(), 70);
    assert_eq!(ZoomPct::reset().get(), 100);
}

#[test]
fn new_clamps_into_range() {
    assert_eq!(ZoomPct::new(10).get(), 70);
    assert_eq!(ZoomPct::new(999).get(), 200);
}

#[test]
fn factor_and_rem_px() {
    assert_eq!(ZoomPct::new(100).factor(), 1.0);
    assert_eq!(ZoomPct::new(100).rem_px(), 16.0);
    assert!((ZoomPct::new(110).rem_px() - 17.6).abs() < 1e-4);
    // Spec: body 11px at 110% = 12.1.
    assert!((11.0_f32 * ZoomPct::new(110).factor() - 12.1).abs() < 1e-4);
}

#[test]
fn from_json_reads_valid() {
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": 130})).get(), 130);
}

#[test]
fn from_json_out_of_range_falls_back_to_100() {
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": 999})).get(), 100);
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": 10})).get(), 100);
}

#[test]
fn from_json_garbage_falls_back_to_100() {
    assert_eq!(ZoomPct::from_json(&json!({})).get(), 100);
    assert_eq!(ZoomPct::from_json(&json!("x")).get(), 100);
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": "130"})).get(), 100);
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": -5})).get(), 100);
    assert_eq!(ZoomPct::from_json(&json!({"zoom_pct": 1.5})).get(), 100);
}

#[test]
fn to_json_round_trips() {
    let z = ZoomPct::new(130);
    assert_eq!(z.to_json(), json!({"zoom_pct": 130}));
    assert_eq!(ZoomPct::from_json(&z.to_json()), z);
}

#[test]
fn label_shows_only_when_not_100() {
    assert_eq!(ZoomPct::new(100).status_label(), None);
    assert_eq!(ZoomPct::new(120).status_label().as_deref(), Some("縮放 120%"));
}

// ---- preferences port (in-memory data source) ----------------------------------------------

use std::sync::Mutex;

use serde_json::Value;

use super::super::bridge::{ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use crate::store::event_query::{EventPage, EventQuery};

#[derive(Default)]
struct MemPrefs {
    stored: Mutex<Option<Value>>,
    fail_load: bool,
    fail_save: bool,
}

impl ReadOnlyDataSource for MemPrefs {
    fn drain_updates(&self) -> Vec<SourceUpdate> {
        Vec::new()
    }
    fn request_refresh(&self) -> RefreshRequest {
        RefreshRequest::IgnoredInProgress
    }
    fn refresh_in_progress(&self) -> bool {
        false
    }
    fn load_events(&self, _: &EventQuery) -> Result<EventPage, String> {
        Err("unused".into())
    }
    fn load_ui_prefs(&self) -> Result<Option<Value>, String> {
        if self.fail_load { Err("db down".into()) } else { Ok(self.stored.lock().unwrap().clone()) }
    }
    fn save_ui_prefs(&self, prefs: &Value) -> Result<(), String> {
        if self.fail_save {
            return Err("disk full".into());
        }
        *self.stored.lock().unwrap() = Some(prefs.clone());
        Ok(())
    }
}

#[test]
fn load_zoom_missing_is_100() {
    assert_eq!(load_zoom(&MemPrefs::default()).get(), 100);
}

#[test]
fn load_zoom_reads_saved_value() {
    let s = MemPrefs::default();
    save_zoom(&s, ZoomPct::new(130));
    assert_eq!(*s.stored.lock().unwrap(), Some(json!({"zoom_pct": 130})));
    assert_eq!(load_zoom(&s).get(), 130);
}

#[test]
fn load_zoom_corrupt_value_is_100() {
    let s = MemPrefs { stored: Mutex::new(Some(json!({"zoom_pct": 999}))), ..Default::default() };
    assert_eq!(load_zoom(&s).get(), 100);
}

#[test]
fn load_zoom_read_error_is_100() {
    let s = MemPrefs { fail_load: true, ..Default::default() };
    assert_eq!(load_zoom(&s).get(), 100);
}

#[test]
fn save_failure_does_not_panic_or_change_zoom() {
    let s = MemPrefs { fail_save: true, ..Default::default() };
    save_zoom(&s, ZoomPct::new(130)); // only logs a warning
    assert_eq!(*s.stored.lock().unwrap(), None);
}

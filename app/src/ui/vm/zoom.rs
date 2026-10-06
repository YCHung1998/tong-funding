//! UI zoom (change: ui-font-zoom). Pure: no GPUI types. The shell turns [`ZoomPct::rem_px`] into
//! `Theme.font_size`, which gpui-component's Root applies as the window rem size every frame.

use serde_json::Value;

pub const MIN_PCT: u16 = 70;
pub const MAX_PCT: u16 = 200;
pub const STEP_PCT: u16 = 10;
pub const DEFAULT_PCT: u16 = 100;
/// `config` key of the UI preferences.
pub const KEY_UI_PREFS: &str = "ui_prefs";
/// The rem base at 100% (gpui-component's default `Theme.font_size`).
pub const BASE_REM_PX: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomPct(u16);

impl Default for ZoomPct {
    fn default() -> Self {
        ZoomPct(DEFAULT_PCT)
    }
}

impl ZoomPct {
    pub fn new(pct: u16) -> ZoomPct {
        ZoomPct(pct.clamp(MIN_PCT, MAX_PCT))
    }
    pub fn reset() -> ZoomPct {
        ZoomPct::default()
    }
    pub fn get(self) -> u16 {
        self.0
    }
    pub fn step_in(self) -> ZoomPct {
        ZoomPct::new(self.0 + STEP_PCT)
    }
    pub fn step_out(self) -> ZoomPct {
        ZoomPct::new(self.0.saturating_sub(STEP_PCT))
    }
    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }
    pub fn rem_px(self) -> f32 {
        BASE_REM_PX * self.factor()
    }
    pub fn from_json(v: &Value) -> ZoomPct {
        match v.get("zoom_pct").and_then(Value::as_u64) {
            Some(n) if (u64::from(MIN_PCT)..=u64::from(MAX_PCT)).contains(&n) => ZoomPct(n as u16),
            _ => ZoomPct::default(),
        }
    }
    pub fn to_json(self) -> Value {
        serde_json::json!({ "zoom_pct": self.0 })
    }
    pub fn status_label(self) -> Option<String> {
        (self.0 != DEFAULT_PCT).then(|| format!("縮放 {}%", self.0))
    }
}

/// Startup value: any read failure or bad stored value gives 100% (never blocks startup).
pub fn load_zoom(source: &dyn super::bridge::ReadOnlyDataSource) -> ZoomPct {
    match source.load_ui_prefs() {
        Ok(Some(v)) => ZoomPct::from_json(&v),
        Ok(None) => ZoomPct::default(),
        Err(e) => {
            eprintln!("warning: ui_prefs unreadable, zoom 100%: {e}");
            ZoomPct::default()
        }
    }
}

/// A write failure only logs a warning; the zoom in effect is unchanged.
pub fn save_zoom(source: &dyn super::bridge::ReadOnlyDataSource, zoom: ZoomPct) {
    if let Err(e) = source.save_ui_prefs(&zoom.to_json()) {
        eprintln!("warning: ui_prefs not saved: {e}");
    }
}

#[cfg(test)]
#[path = "zoom_tests.rs"]
mod tests;

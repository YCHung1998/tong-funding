//! Cmd/Ctrl +/-/0 zoom (change: ui-font-zoom). The logic (steps, bounds, persistence) is the pure
//! `zoom` module; this glue maps a [`ZoomPct`] onto `Theme.font_size`, which gpui-component's Root
//! applies as the window rem size every frame (so every `rems` length in the app scales).

use std::sync::Arc;

use gpui_kit::component::theme::Theme;
use gpui_kit::*;

use super::bridge::ReadOnlyDataSource;
use super::zoom::{self, ZoomPct};

actions!(tong, [ZoomIn, ZoomOut, ZoomReset]);

/// The zoom in effect (read by the status bar).
pub struct ZoomState(pub ZoomPct);

impl Global for ZoomState {}

pub fn current(cx: &App) -> ZoomPct {
    cx.try_global::<ZoomState>().map(|z| z.0).unwrap_or_default()
}

fn apply(zoom: ZoomPct, cx: &mut App) {
    cx.set_global(ZoomState(zoom));
    Theme::update(cx, |t| t.font_size = px(zoom.rem_px()));
    cx.refresh_windows();
}

/// Loads the saved zoom, applies it, and binds the keys and handlers.
pub fn install(source: Arc<dyn ReadOnlyDataSource>, cx: &mut App) {
    apply(zoom::load_zoom(source.as_ref()), cx);
    let mut keys = Vec::new();
    for m in ["cmd", "ctrl"] {
        keys.push(KeyBinding::new(&format!("{m}-="), ZoomIn, None));
        keys.push(KeyBinding::new(&format!("{m}-+"), ZoomIn, None));
        keys.push(KeyBinding::new(&format!("{m}--"), ZoomOut, None));
        keys.push(KeyBinding::new(&format!("{m}-0"), ZoomReset, None));
    }
    cx.bind_keys(keys);
    let change = move |cx: &mut App, f: fn(ZoomPct) -> ZoomPct| {
        let next = f(current(cx));
        apply(next, cx);
        zoom::save_zoom(source.as_ref(), next);
    };
    let c = change.clone();
    cx.on_action(move |_: &ZoomIn, cx| c(cx, ZoomPct::step_in));
    let c = change.clone();
    cx.on_action(move |_: &ZoomOut, cx| c(cx, ZoomPct::step_out));
    cx.on_action(move |_: &ZoomReset, cx| change(cx, |_| ZoomPct::reset()));
}

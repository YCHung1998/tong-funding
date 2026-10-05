mod ui;

use gpui_kit::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        ui::component_theme::apply(cx);
        ui::fonts::register(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(800.0)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("Funding Monitor".into()), ..Default::default() }),
            ..Default::default()
        };
        // Developer-only spikes (bootstrap-gpui-shell tasks 4.1 / 4.2); see ui/bench.rs.
        match args.get(1).map(String::as_str) {
            Some("--bench-table") => {
                let hz: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
                let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
                gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| ui::bench::TableBench::new(hz, secs, window, cx)))
                    .expect("failed to open window");
            }
            Some("--bench-donut") => {
                gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| ui::bench::DonutBench)).expect("failed to open window");
            }
            _ => {
                gpui_kit::open_window(options, cx, |_, cx| cx.new(ui::shell::Shell::new)).expect("failed to open window");
            }
        }
        cx.activate(true);
    });
}

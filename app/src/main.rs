mod engine;
mod exchange;
mod funding;
mod ports;
mod store;
mod ui;

use gpui_kit::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Headless subcommands run before any window is created.
    if args.get(1).map(String::as_str) == Some(store::import_cli::SUBCOMMAND) {
        std::process::exit(store::import_cli::run(&args[2..], &mut std::io::stdout(), &mut std::io::stderr()));
    }
    if args.get(1).map(String::as_str) == Some(store::secrets_cli::SUBCOMMAND) {
        let code = store::secrets_cli::run(&args[2..], &mut std::io::stdin().lock(), &mut std::io::stdout(), &mut std::io::stderr());
        std::process::exit(code);
    }
    if args.get(1).map(String::as_str) == Some(store::config_cli::SUBCOMMAND) {
        std::process::exit(store::config_cli::run(&args[2..], &mut std::io::stdout(), &mut std::io::stderr()));
    }
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        ui::component_theme::apply(cx);
        ui::fonts::register(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1600.0), px(1000.0)), cx))),
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
                // Composition root: ONE store instance for the data source and the engine; the
                // engine starts with the window (headless subcommands above never reach this).
                let db = store::db::Db::open_default(std::sync::Arc::new(ports::SystemClock)).ok();
                let live = ui::live::LiveSource::start(db);
                let source: std::sync::Arc<dyn ui::bridge::ReadOnlyDataSource> = live.clone();
                let sink: std::sync::Arc<dyn ui::bridge::CommandSink> = live;
                gpui_kit::open_window(options, cx, move |window, cx| cx.new(|cx| ui::shell::Shell::new(source, sink, window, cx))).expect("failed to open window");
            }
        }
        cx.activate(true);
    });
}

//! Developer-only spikes for change `bootstrap-gpui-shell` (tasks 4.1 / 4.2):
//! `tong-funding --bench-table <hz> [seconds]` and `tong-funding --bench-donut`.
//! Not part of the normal UI; results are recorded in the change's design.md.

use std::time::{Duration, Instant};

use gpui_kit::component::chart::PieChart;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::*;

use super::frame_stats::{drops, split_paused, summarize};
use super::theme::{self, funding_tone};

const ROWS: usize = 528; // trading USDT perpetuals on Binance, measured 2026-10-05
const WARMUP_FRAMES: usize = 30;

/// Cheap deterministic pseudo-random walk so no `rand` dependency is needed.
struct Lcg(u64);
impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f64) / ((1u64 << 31) as f64)
    }
}

struct Row {
    symbol: String,
    binance: f64,
    bybit: f64,
}

pub struct BenchRows {
    rows: Vec<Row>,
    rng: Lcg,
}

impl BenchRows {
    fn new() -> Self {
        let mut rng = Lcg(42);
        let rows = (0..ROWS)
            .map(|i| Row {
                symbol: format!("SYM{i:03}USDT"),
                binance: (rng.next_f64() - 0.5) * 0.2,
                bybit: (rng.next_f64() - 0.5) * 0.2,
            })
            .collect();
        BenchRows { rows, rng }
    }

    /// One data update: every row's rates drift, like a full-market refresh.
    fn tick(&mut self) {
        for r in &mut self.rows {
            r.binance += (self.rng.next_f64() - 0.5) * 0.01;
            r.bybit += (self.rng.next_f64() - 0.5) * 0.01;
        }
    }
}

const COLS: [(&str, &str); 8] = [
    ("rank", "Rank"),
    ("symbol", "Symbol"),
    ("binance", "Binance Rate %"),
    ("bybit", "Bybit Rate %"),
    ("spread", "Spread %"),
    ("net", "Net Edge %"),
    ("dir", "Direction"),
    ("ok", "Qualified"),
];

impl TableDelegate for BenchRows {
    fn columns_count(&self, _: &App) -> usize {
        COLS.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        Column::new(COLS[ix].0, COLS[ix].1).width(px(130.0))
    }
    fn render_td(&mut self, row: usize, col: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let r = &self.rows[row];
        let spread = (r.binance - r.bybit).abs();
        let (text, color) = match col {
            0 => (format!("{:02}", row + 1), theme::TEXT_MUTED),
            1 => (r.symbol.clone(), theme::TEXT_PRIMARY),
            2 => (format!("{:+.3}", r.binance), funding_tone(r.binance).color()),
            3 => (format!("{:+.3}", r.bybit), funding_tone(r.bybit).color()),
            4 => (format!("{spread:.3}"), theme::TEXT_SECONDARY),
            5 => (format!("{:+.3}", spread - 0.2), funding_tone(spread - 0.2).color()),
            6 => (if r.binance < r.bybit { "L Binance / S Bybit" } else { "L Bybit / S Binance" }.to_string(), theme::TEXT_SECONDARY),
            _ => (if spread > 0.1 { "✓" } else { "-" }.to_string(), theme::TEXT_SECONDARY),
        };
        div().text_color(rgb(color)).child(text)
    }
}

pub struct TableBench {
    table: Entity<TableState<BenchRows>>,
    hz: f64,
    started: Instant,
    duration: Duration,
    last_frame: Option<Instant>,
    intervals_ms: Vec<f64>,
}

impl TableBench {
    pub fn new(hz: f64, seconds: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| TableState::new(BenchRows::new(), window, cx));
        let ticking = table.downgrade();
        if hz > 0.0 {
            cx.spawn(async move |_, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs_f64(1.0 / hz)).await;
                    if ticking
                        .update(cx, |t, cx| {
                            t.delegate_mut().tick();
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        TableBench {
            table,
            hz,
            started: Instant::now(),
            duration: Duration::from_secs(seconds),
            last_frame: None,
            intervals_ms: Vec::new(),
        }
    }
}

impl Render for TableBench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            self.intervals_ms.push(now.duration_since(prev).as_secs_f64() * 1000.0);
        }
        self.last_frame = Some(now);

        if now.duration_since(self.started) >= self.duration {
            let samples = &self.intervals_ms[self.intervals_ms.len().min(WARMUP_FRAMES)..];
            let running = split_paused(samples);
            match summarize(&running) {
                Some(s) => {
                    let d = drops(samples, s.p50_ms);
                    println!(
                        "BENCH rows={ROWS} update_hz={} frames={} p50_ms={:.2} p95_ms={:.2} max_ms={:.2} dropped={} paused={}",
                        self.hz, s.frames, s.p50_ms, s.p95_ms, s.max_ms, d.dropped, d.paused
                    );
                }
                None => println!("BENCH no frames recorded"),
            }
            cx.quit();
        }
        // Worst case on purpose: ask for a redraw every frame, whether or not data changed.
        window.request_animation_frame();

        div()
            .size_full()
            .bg(rgb(theme::BG_DEEPEST))
            .child(DataTable::new(&self.table).stripe(true).bordered(true))
    }
}

// ---- donut spike -----------------------------------------------------------

struct Slice {
    name: &'static str,
    value: f64,
    color: u32,
}

pub struct DonutBench;

impl Render for DonutBench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Binance asset distribution from the Figma dashboard (USDT / BTC / ETH / 合約).
        let data = vec![
            Slice { name: "USDT", value: 16_200.0, color: theme::ACCENT },
            Slice { name: "BTC", value: 3_010.0, color: theme::WARNING },
            Slice { name: "ETH", value: 1_490.0, color: theme::INFO },
            Slice { name: "合約", value: 1_300.0, color: theme::POSITIVE },
        ];
        div()
            .size_full()
            .bg(rgb(theme::BG_DEEPEST))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child("donut spike: Binance 22,000.00 USDT")
            .child(
                div().w(px(360.0)).h(px(300.0)).child(
                    PieChart::new(data)
                        .value(|d: &Slice| d.value as f32)
                        .color(|d: &Slice| rgb(d.color))
                        .inner_radius(70.0)
                        .outer_radius(110.0),
                ),
            )
    }
}

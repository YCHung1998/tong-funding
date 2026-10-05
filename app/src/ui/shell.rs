//! Main window shell: header (dual clock), sidebar, routed content, status bar.
//! All decisions (order, labels, defaults, clock math) live in `nav` / `clock` / `status`,
//! which are unit-tested; this file only lays them out.

use std::time::Duration;

use gpui_kit::*;

use super::clock::{now_unix_secs, read_clock};
use super::fonts::app_font;
use super::nav::{Page, DEBUG_WARNING};
use super::status::{Connection, StatusModel, ENVIRONMENT_LABEL};
use super::theme;

pub struct Shell {
    page: Page,
    now_secs: i64,
    status: StatusModel,
}

impl Shell {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let shell = Shell {
            page: Page::default_page(),
            now_secs: now_unix_secs(),
            status: StatusModel::default(),
        };
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this
                    .update(cx, |s, cx| {
                        s.now_secs = now_unix_secs();
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        shell
    }

    fn header(&self) -> Div {
        let c = read_clock(self.now_secs);
        let clock = |label: &str, date: String, time: String| {
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label.to_string()))
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(date))
                .child(div().text_color(rgb(theme::TEXT_PRIMARY)).child(time))
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .px_4()
            .bg(rgb(theme::BG_BASE))
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_color(rgb(theme::ACCENT)).text_size(px(14.0)).child("◈ Funding Monitor"))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(theme::BG_CARD))
                            .text_color(rgb(theme::WARNING))
                            .child(ENVIRONMENT_LABEL),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_6()
                    .child(clock("UTC", c.utc_date, c.utc_time))
                    .child(clock("TAIPEI · UTC+8", c.taipei_date, c.taipei_time)),
            )
    }

    fn nav_item(&self, idx: usize, page: Page, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.page == page;
        div()
            .id(("nav", idx))
            .flex()
            .flex_col()
            .px_3()
            .py_2()
            .cursor_pointer()
            .border_l_2()
            .border_color(rgb(if selected { theme::ACCENT } else { theme::BG_BASE }))
            .bg(rgb(if selected { theme::BG_CARD } else { theme::BG_BASE }))
            .hover(|s| s.bg(rgb(theme::BG_SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.page = page;
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY }))
                    .child(page.zh()),
            )
            .child(div().text_size(px(10.0)).text_color(rgb(theme::TEXT_MUTED)).child(page.en()))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut bar = div()
            .flex()
            .flex_col()
            .w(px(200.0))
            .flex_none()
            .bg(rgb(theme::BG_BASE))
            .border_r_1()
            .border_color(rgb(theme::BORDER));
        for (idx, page) in Page::ALL.into_iter().enumerate() {
            if page.is_debug() {
                bar = bar
                    .child(div().h(px(1.0)).mx_3().my_2().bg(rgb(theme::BORDER)))
                    .child(
                        div()
                            .px_3()
                            .pb_1()
                            .text_size(px(10.0))
                            .text_color(rgb(theme::WARNING))
                            .child(format!("⚠ {DEBUG_WARNING}")),
                    );
            }
            bar = bar.child(self.nav_item(idx, page, cx));
        }
        bar
    }

    fn content(&self) -> Div {
        div()
            .flex_1()
            .p_6()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(21.0)).text_color(rgb(theme::TEXT_PRIMARY)).child(self.page.zh()))
            .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(self.page.en()))
            .child(div().mt_4().text_color(rgb(theme::TEXT_MUTED)).child("（尚未實作）"))
    }

    fn status_bar(&self) -> Div {
        let mut bar = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_4()
            .bg(rgb(theme::BG_BASE))
            .border_t_1()
            .border_color(rgb(theme::BORDER))
            .text_size(px(10.0));

        let mut left = div().flex().items_center().gap_4().child(
            div()
                .px_2()
                .rounded_sm()
                .bg(rgb(theme::BG_CARD))
                .text_color(rgb(theme::ACCENT))
                .child(self.status.mode.label()),
        );
        for (name, conn) in &self.status.exchanges {
            let dot = if *conn == Connection::Connected { theme::POSITIVE } else { theme::TEXT_MUTED };
            left = left.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.0)).rounded_full().bg(rgb(dot)))
                    .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(*name))
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child(conn.label())),
            );
        }
        bar = bar.child(left);
        bar.child(
            div()
                .flex()
                .gap_1()
                .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Kill switch"))
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(self.status.kill_switch.label())),
        )
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_DEEPEST))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .text_size(px(theme::FONT_SIZE_BODY))
            .font(app_font(FontWeight::NORMAL))
            .child(self.header())
            .child(div().flex().flex_1().child(self.sidebar(cx)).child(self.content()))
            .child(self.status_bar())
    }
}

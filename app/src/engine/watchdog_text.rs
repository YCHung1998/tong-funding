//! naked-leg-watchdog: every sentence the user reads about it, from ONE place. The risk page's rule
//! block, the manual-handling card of the staged-orders page and the notifications all take their
//! words from here, so the three can never disagree. Pure: numbers come in as arguments (the page
//! passes the real `order_timeout_seconds`, the constants of `watchdog` give the 5 s and the 3 tries).

use tong_funding_core::types::{Decimal, Exchange};

use super::ports::Leg;

/// "多腿" / "空腿".
pub fn leg_word(leg: Leg) -> &'static str {
    match leg {
        Leg::Long => "多腿",
        Leg::Short => "空腿",
    }
}

/// The sentence for a flatten that really happened. The spec fixes its wording
/// (「系統已自動平掉多腿，請確認兩所皆無部位後按『人工確認已平倉』」).
pub fn flattened_sentence(leg: Leg) -> String {
    format!("系統已自動平掉{}，請確認兩所皆無部位後按「人工確認已平倉」。", leg_word(leg))
}

/// What exactly was sent: exchange, direction, quantity.
pub fn flattened_detail(leg: Leg, exchange: Exchange, quantity: Decimal) -> String {
    // closing a long position sells, closing a short position buys
    let verb = match leg {
        Leg::Long => "賣出",
        Leg::Short => "買進",
    };
    format!("明細：{} {verb} {}（只減倉市價單）。", exchange.name(), quantity.normalize())
}

/// The sentence for a round that could not prove the situation: nothing was sent, new entries are stopped.
pub fn unknown_sentence(reason: &str) -> String {
    format!("查證結果不明（{reason}），系統沒有送出任何單，已停止開新倉並通知你，請人工處理。")
}

/// Notification title / body for "the watchdog flattened the bare leg".
pub fn flattened_notice(symbol: &str, pair_id: &str, env: &str, leg: Leg, exchange: Exchange, quantity: Decimal) -> (String, String) {
    (
        format!("tong-funding: {symbol} 看門狗已自動平倉"),
        format!("{pair_id}（{env}）{} {}", flattened_sentence(leg), flattened_detail(leg, exchange, quantity)),
    )
}

/// Notification title / body for "the watchdog could not prove it: nothing sent, new entries stopped".
pub fn locked_notice(symbol: &str, pair_id: &str, env: &str, reason: &str) -> (String, String) {
    (format!("tong-funding: {symbol} 看門狗查證不明，已停止開新倉"), format!("{pair_id}（{env}）{}", unknown_sentence(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn the_flattened_sentence_has_the_wording_of_the_spec() {
        // spec: 該配對的人工處理卡寫明「系統已自動平掉多腿，請確認兩所皆無部位後按『人工確認已平倉』」
        assert_eq!(flattened_sentence(Leg::Long), "系統已自動平掉多腿，請確認兩所皆無部位後按「人工確認已平倉」。");
        assert_eq!(flattened_sentence(Leg::Short), "系統已自動平掉空腿，請確認兩所皆無部位後按「人工確認已平倉」。");
    }

    #[test]
    fn the_detail_names_the_exchange_the_direction_and_the_quantity() {
        assert_eq!(flattened_detail(Leg::Long, Exchange::Binance, dec("0.090")), "明細：Binance 賣出 0.09（只減倉市價單）。");
        assert_eq!(flattened_detail(Leg::Short, Exchange::Bybit, dec("10")), "明細：Bybit 買進 10（只減倉市價單）。");
    }

    #[test]
    fn the_unknown_sentence_says_nothing_was_sent_and_new_entries_are_stopped() {
        let s = unknown_sentence("long position unreadable: timeout");
        assert!(s.contains("查證結果不明") && s.contains("long position unreadable: timeout"), "{s}");
        assert!(s.contains("沒有送出任何單") && s.contains("停止開新倉") && s.contains("人工處理"), "{s}");
    }

    #[test]
    fn the_notices_use_the_same_sentences_as_the_card() {
        let (title, body) = flattened_notice("BTCUSDT", "pid-1", "EXCHANGE_DEMO", Leg::Long, Exchange::Binance, dec("10"));
        assert!(title.contains("BTCUSDT") && title.contains("自動平倉"), "{title}");
        assert!(body.contains("pid-1") && body.contains("EXCHANGE_DEMO"), "{body}");
        assert!(body.contains(&flattened_sentence(Leg::Long)) && body.contains(&flattened_detail(Leg::Long, Exchange::Binance, dec("10"))), "{body}");

        let (title, body) = locked_notice("BTCUSDT", "pid-1", "LIVE", "auto flatten failed 3 times");
        assert!(title.contains("BTCUSDT") && title.contains("停止開新倉"), "{title}");
        assert!(body.contains("pid-1") && body.contains("LIVE") && body.contains(&unknown_sentence("auto flatten failed 3 times")), "{body}");
    }
}

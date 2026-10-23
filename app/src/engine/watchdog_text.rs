//! naked-leg-watchdog: every sentence the user reads about it, from ONE place. The risk page's rule
//! block, the manual-handling card of the staged-orders page and the notifications all take their
//! words from here, so the three can never disagree. Pure: numbers come in as arguments (the page
//! passes the real `order_timeout_seconds`, the constants of `watchdog` give the 5 s and the 3 tries).

use tong_funding_core::types::{Decimal, Exchange};

use super::ports::Leg;
use super::watchdog::{MAX_FLATTEN_ATTEMPTS, WATCHDOG_GRACE_MS};

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

/// Title of the fixed block on the risk page.
pub const RULES_TITLE: &str = "自動處理規則（授權 B）";

/// One run of a paragraph: emphasised runs are bold on the page and plain everywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
}

/// The runs joined, without any emphasis.
pub fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// The approved rule text (authorization B), numbers taken from the real setting and from the
/// constants of `watchdog`: what the system does on its own, when, and what it never does.
pub fn rules_runs(order_timeout_seconds: u32) -> Vec<Run> {
    let wait_seconds = WATCHDOG_GRACE_MS / 1_000;
    let plain_run = |text: String| Run { text, bold: false };
    let bold_run = |text: &str| Run { text: text.to_string(), bold: true };
    vec![
        plain_run("只有在".into()),
        bold_run("確認只有一腿成交"),
        plain_run(format!("時，系統會在訂單逾時 {order_timeout_seconds} 秒後再等 {wait_seconds} 秒，先撤銷另一腿的委託並查證它確實沒有成交，然後以")),
        bold_run("只減倉的市價單"),
        plain_run(format!("平掉已成交那一腿（最多重試 {MAX_FLATTEN_ATTEMPTS} 次）。系統")),
        bold_run("不會"),
        plain_run("開新倉、加碼、補另一腿，或動到這個配對以外的持倉，也不會改變配對狀態（仍需你確認已平倉）。查證結果不明或重試用完時".into()),
        bold_run("不會動作"),
        plain_run("，改為停止開新倉並通知你。".into()),
    ]
}

/// [`rules_runs`] without emphasis.
pub fn rules_plain(order_timeout_seconds: u32) -> String {
    plain(&rules_runs(order_timeout_seconds))
}

/// A line to show under the rules when an exchange overrides `order_timeout_seconds`: the pair then
/// waits for the smaller of its two legs' values, so the number in the rules is not the whole story.
pub fn rules_override_note(overrides: &[(Exchange, u32)]) -> Option<String> {
    if overrides.is_empty() {
        return None;
    }
    let list = overrides.iter().map(|(exchange, seconds)| format!("{} {seconds} 秒", exchange.name())).collect::<Vec<_>>().join("、");
    Some(format!("提醒：{list} 另有訂單逾時覆寫，配對實際取兩腿中較小的值（見下方「配對生效值預覽」）。"))
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

    // ---- 3.1 the rules text of the risk page ----

    const APPROVED_20: &str = "只有在確認只有一腿成交時，系統會在訂單逾時 20 秒後再等 5 秒，先撤銷另一腿的委託並查證它確實沒有成交，然後以只減倉的市價單平掉已成交那一腿（最多重試 3 次）。系統不會開新倉、加碼、補另一腿，或動到這個配對以外的持倉，也不會改變配對狀態（仍需你確認已平倉）。查證結果不明或重試用完時不會動作，改為停止開新倉並通知你。";

    #[test]
    fn the_rules_text_is_the_approved_draft_with_the_real_numbers() {
        assert_eq!(rules_plain(20), APPROVED_20);
        assert_eq!(RULES_TITLE, "自動處理規則（授權 B）");
    }

    #[test]
    fn the_order_timeout_in_the_text_follows_the_setting() {
        // spec: order_timeout_seconds 為 20 -> 「訂單逾時 20 秒後再等 5 秒」
        for n in [1_u32, 7, 15, 20, 120] {
            let text = rules_plain(n);
            assert!(text.contains(&format!("訂單逾時 {n} 秒後再等 5 秒")), "{n}: {text}");
        }
        assert_eq!(rules_plain(15).replace("15 秒後", "20 秒後"), rules_plain(20), "nothing else depends on it");
    }

    #[test]
    fn the_emphasis_is_on_exactly_the_four_phrases_of_the_draft() {
        let bold: Vec<String> = rules_runs(20).into_iter().filter(|r| r.bold).map(|r| r.text).collect();
        assert_eq!(bold, vec!["確認只有一腿成交", "只減倉的市價單", "不會", "不會動作"]);
        assert!(rules_runs(20).iter().all(|r| !r.text.is_empty()), "no empty runs");
    }

    #[test]
    fn the_rules_say_when_it_acts_what_it_does_what_it_never_does_and_what_happens_when_unsure() {
        let t = rules_plain(15);
        for needle in [
            "確認只有一腿成交", // when
            "再等 5 秒", // the wait
            "撤銷另一腿的委託", "查證它確實沒有成交", "只減倉的市價單", "最多重試 3 次", // what it does
            "不會開新倉、加碼、補另一腿", "配對以外的持倉", "不會改變配對狀態", "仍需你確認已平倉", // what it never does
            "查證結果不明或重試用完時不會動作", "停止開新倉並通知你", // when unsure
        ] {
            assert!(t.contains(needle), "missing {needle:?} in {t}");
        }
    }

    #[test]
    fn a_timeout_override_gets_a_note_and_no_override_gets_none() {
        assert_eq!(rules_override_note(&[]), None);
        let note = rules_override_note(&[(Exchange::Bybit, 7), (Exchange::Binance, 20)]).unwrap();
        assert!(note.contains("Binance 20 秒") && note.contains("Bybit 7 秒") && note.contains("較小"), "{note}");
    }
}

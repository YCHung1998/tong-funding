//! naked-leg-watchdog: every sentence the user reads about it, from ONE place. The risk page's rule
//! block, the manual-handling card of the staged-orders page and the notifications all take their
//! words from here, so the three can never disagree. Pure: numbers come in as arguments (the page
//! passes the real `order_timeout_seconds`, the constants of `watchdog` give the 5 s and the 3 tries).

use tong_funding_core::types::{Decimal, Exchange};

use super::ports::Leg;
use super::watchdog::{MAX_CANCEL_WAIT_ROUNDS, MAX_FLATTEN_ATTEMPTS, MAX_READ_FAIL_ROUNDS, SentOrder, Status, WATCHDOG_GRACE_MS};

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

/// What the system does to TELL the user, said as it is: an alert in the app and a line in the system
/// log. There is no macOS system notification yet (`LogNotifier`), so no text may promise one.
pub const NOTIFY_NOTE: &str = "在 App 內顯示警示並記錄於系統日誌（尚未接上 macOS 系統通知）";

/// The sentence for a round that could not prove the situation: new entries are stopped. When the
/// watchdog had already sent flatten orders in this entry they are listed (one per order, with the
/// fill its result reported); only when it sent none does the sentence say that nothing was sent.
pub fn unknown_sentence(reason: &str, sent: &[SentOrder]) -> String {
    if sent.is_empty() {
        return format!("查證結果不明（{reason}），系統沒有送出任何單，已停止開新倉，並{NOTIFY_NOTE}，請人工處理。");
    }
    let list = sent.iter().enumerate().map(|(i, o)| sent_line(i + 1, o)).collect::<Vec<_>>().join("；");
    format!("查證結果不明（{reason}）。看門狗已送出 {} 張平倉單：{list}。已停止開新倉，並{NOTIFY_NOTE}；請先到交易所確認兩腿實際持倉，再人工處理。", sent.len())
}

/// One flatten order of an entry: its number, leg, exchange, direction, quantity and the fill reported.
fn sent_line(n: usize, o: &SentOrder) -> String {
    // closing a long position sells, closing a short position buys
    let verb = match o.leg {
        Leg::Long => "賣出",
        Leg::Short => "買進",
    };
    let filled = o.filled.map_or("成交量未確認".to_string(), |f| format!("成交 {}", f.normalize()));
    format!("第 {n} 張：{} {} {verb} {}，{filled}", leg_word(o.leg), o.exchange, o.quantity.normalize())
}

/// Notification title / body for "the watchdog flattened the bare leg".
pub fn flattened_notice(symbol: &str, pair_id: &str, env: &str, leg: Leg, exchange: Exchange, quantity: Decimal) -> (String, String) {
    (
        format!("tong-funding: {symbol} 看門狗已自動平倉"),
        format!("{pair_id}（{env}）{} {}", flattened_sentence(leg), flattened_detail(leg, exchange, quantity)),
    )
}

/// Notification title / body for "the watchdog could not prove it: nothing sent, new entries stopped".
pub fn locked_notice(symbol: &str, pair_id: &str, env: &str, reason: &str, sent: &[SentOrder]) -> (String, String) {
    (format!("tong-funding: {symbol} 看門狗查證不明，已停止開新倉"), format!("{pair_id}（{env}）{}", unknown_sentence(reason, sent)))
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
        plain_run(format!("時（另一腿已平倉、只剩一腿有部位時也適用），系統會在訂單逾時 {order_timeout_seconds} 秒後再等 {wait_seconds} 秒，先撤銷另一腿的委託並查證它確實沒有成交，然後以")),
        bold_run("只減倉的市價單"),
        plain_run(format!("平掉已成交那一腿（最多送出 {MAX_FLATTEN_ATTEMPTS} 次）。系統")),
        bold_run("不會"),
        plain_run("開新倉、加碼、補另一腿，或動到這個配對以外的持倉，也不會改變配對狀態（仍需你確認已平倉）。查證結果不明或送單用完時".into()),
        bold_run("不會動作"),
        plain_run(format!("，改為停止開新倉，並{NOTIFY_NOTE}。停止開新倉時，提前出場也會暫停，排程出場照常。")),
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

/// The line under the heading of the manual-handling card: what the system does NOT do (repair,
/// move the pair), and where the rules and each pair's watchdog status are.
pub const CARD_INTRO: &str = "系統不會補買、補賣另一腿，也不會改變配對狀態；只有確認只有一腿成交時，看門狗才會自動平掉那一腿（規則見風控設定頁「自動處理規則（授權 B）」）。各配對的看門狗狀態在下方。";

/// The sentence the pair-alert notification ends with (instead of "Nothing is done automatically"):
/// what the system will and will not do on its own, in one line.
pub fn pair_alert_note() -> String {
    format!(
        "系統不會補單；只有確認只有一腿成交時，看門狗才會在訂單逾時後再等 {} 秒，撤銷並查證另一腿，再以只減倉市價單平掉已成交那一腿，其餘情況不會動作（規則見風控設定頁）。",
        WATCHDOG_GRACE_MS / 1_000
    )
}

/// `HH:MM:SS` of a time in ms since the epoch, in UTC.
fn hms(ms: i64) -> String {
    let secs = (ms / 1_000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", secs / 3_600, secs % 3_600 / 60, secs % 60)
}

/// The lines the manual-handling card shows for the watchdog's status: what it did, what it will do
/// next, and what the user has to do. `exchanges` = (long, short) of the pair.
pub fn status_lines(status: &Status, exchanges: (Exchange, Exchange), now_ms: i64) -> Vec<String> {
    let wait_seconds = WATCHDOG_GRACE_MS / 1_000;
    let exchange_of = |leg: Leg| match leg {
        Leg::Long => exchanges.0,
        Leg::Short => exchanges.1,
    };
    match status {
        Status::Waiting { due_ms: Some(due) } if *due > now_ms => {
            vec![format!("看門狗等待中：將在 {} UTC 查證（訂單逾時後再等 {wait_seconds} 秒）；只有確認只有一腿成交時才會自動平掉該腿。", hms(*due))]
        }
        Status::Waiting { due_ms: Some(_) } => vec!["看門狗查證中…".to_string()],
        Status::Waiting { due_ms: None } => vec![format!("看門狗等待中：訂單逾時後再等 {wait_seconds} 秒才會查證；只有確認只有一腿成交時才會自動平掉該腿。")],
        Status::WaitingCancel { waited, next_ms } => vec![format!(
            "看門狗：另一腿的委託尚未終結，已送出撤單，將在 {} UTC 再查證（第 {waited} 輪等待，累計 {MAX_CANCEL_WAIT_ROUNDS} 輪未終結就視為查證不明）。",
            hms(*next_ms)
        )],
        Status::Flattening { leg, quantity, attempt } => vec![format!(
            "看門狗已送出{}的只減倉平倉單（數量 {}，第 {attempt} 次，最多送出 {MAX_FLATTEN_ATTEMPTS} 次），等待成交與查證。",
            leg_word(*leg),
            quantity.normalize()
        )],
        Status::Retrying { attempt, reason, next_ms } => vec![format!(
            "看門狗自動平倉第 {attempt} 次未成功（{reason}），將在 {} UTC 重新查證；最多送出 {MAX_FLATTEN_ATTEMPTS} 次，用完會停止開新倉，並{NOTIFY_NOTE}。",
            hms(*next_ms)
        )],
        Status::Flattened { leg, quantity } => vec![flattened_sentence(*leg), flattened_detail(*leg, exchange_of(*leg), *quantity)],
        Status::Hedged { long, short } => {
            let sizes = match (long, short) {
                (Some(l), Some(s)) => format!("：多腿 {}、空腿 {}", l.normalize(), s.normalize()),
                _ => String::new(),
            };
            // both legs hold something, but nothing says the two quantities are equal
            vec![format!("兩腿都有部位（可能不平衡）{sizes}，系統不會送單，請人工處理。")]
        }
        Status::Flat => vec!["兩腿都沒有部位，系統不會送單，請人工處理；確認兩所皆無部位後按「人工確認已平倉」。".to_string()],
        Status::Unknown { reason, sent } => vec![unknown_sentence(reason, sent)],
        Status::ReadRetry { failed, reason, next_ms } => vec![format!(
            "看門狗查證時讀取失敗（{reason}），第 {failed} 輪；將在 {} UTC 再查證，累計 {MAX_READ_FAIL_ROUNDS} 輪讀取失敗就視為查證不明並停止開新倉。",
            hms(*next_ms)
        )],
        Status::Skipped { reason } => vec![format!("看門狗不處理這個配對：{reason}")],
    }
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

    const NOTE: &str = "在 App 內顯示警示並記錄於系統日誌（尚未接上 macOS 系統通知）";

    fn sent(leg: Leg, exchange: &str, quantity: &str, filled: Option<&str>) -> SentOrder {
        SentOrder { leg, exchange: exchange.into(), quantity: dec(quantity), client_order_id: "c".into(), filled: filled.map(dec) }
    }

    #[test]
    fn the_unknown_sentence_without_a_sent_order_says_nothing_was_sent_and_new_entries_are_stopped() {
        let s = unknown_sentence("long position unreadable: timeout", &[]);
        assert!(s.contains("查證結果不明") && s.contains("long position unreadable: timeout"), "{s}");
        assert!(s.contains("沒有送出任何單") && s.contains("停止開新倉") && s.contains("人工處理"), "{s}");
        assert!(s.contains(NOTE) && !s.contains("通知你"), "no promise of a notification that is not wired: {s}");
    }

    #[test]
    fn the_unknown_sentence_after_sent_orders_lists_each_order_with_its_fill_and_never_says_nothing_was_sent() {
        // review round 1 (A8): "auto flatten failed 3 times" is Unknown AFTER three orders went out
        let orders = [sent(Leg::Long, "Binance", "10", Some("4")), sent(Leg::Long, "Binance", "6", Some("0")), sent(Leg::Long, "Binance", "6", None)];
        let s = unknown_sentence("auto flatten failed 3 times", &orders);
        assert!(!s.contains("沒有送出任何單"), "{s}");
        assert!(s.contains("已送出 3 張平倉單"), "{s}");
        assert!(s.contains("第 1 張：多腿 Binance 賣出 10，成交 4") && s.contains("第 2 張：多腿 Binance 賣出 6，成交 0") && s.contains("第 3 張：多腿 Binance 賣出 6，成交量未確認"), "{s}");
        assert!(s.contains("停止開新倉") && s.contains(NOTE) && s.contains("確認") && s.contains("人工處理"), "{s}");
        // a short leg is closed by buying
        let s = unknown_sentence("x", &[sent(Leg::Short, "Bybit", "0.5", Some("0.5"))]);
        assert!(s.contains("已送出 1 張平倉單") && s.contains("第 1 張：空腿 Bybit 買進 0.5，成交 0.5"), "{s}");
    }

    #[test]
    fn the_notices_use_the_same_sentences_as_the_card() {
        let (title, body) = flattened_notice("BTCUSDT", "pid-1", "EXCHANGE_DEMO", Leg::Long, Exchange::Binance, dec("10"));
        assert!(title.contains("BTCUSDT") && title.contains("自動平倉"), "{title}");
        assert!(body.contains("pid-1") && body.contains("EXCHANGE_DEMO"), "{body}");
        assert!(body.contains(&flattened_sentence(Leg::Long)) && body.contains(&flattened_detail(Leg::Long, Exchange::Binance, dec("10"))), "{body}");

        let orders = [sent(Leg::Long, "Binance", "10", Some("0"))];
        let (title, body) = locked_notice("BTCUSDT", "pid-1", "LIVE", "auto flatten failed 3 times", &orders);
        assert!(title.contains("BTCUSDT") && title.contains("停止開新倉"), "{title}");
        assert!(body.contains("pid-1") && body.contains("LIVE") && body.contains(&unknown_sentence("auto flatten failed 3 times", &orders)), "{body}");
    }

    // ---- 3.1 the rules text of the risk page ----

    const APPROVED_20: &str = "只有在確認只有一腿成交時（另一腿已平倉、只剩一腿有部位時也適用），系統會在訂單逾時 20 秒後再等 5 秒，先撤銷另一腿的委託並查證它確實沒有成交，然後以只減倉的市價單平掉已成交那一腿（最多送出 3 次）。系統不會開新倉、加碼、補另一腿，或動到這個配對以外的持倉，也不會改變配對狀態（仍需你確認已平倉）。查證結果不明或送單用完時不會動作，改為停止開新倉，並在 App 內顯示警示並記錄於系統日誌（尚未接上 macOS 系統通知）。停止開新倉時，提前出場也會暫停，排程出場照常。";

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
            "撤銷另一腿的委託", "查證它確實沒有成交", "只減倉的市價單", "最多送出 3 次", // what it does
            "另一腿已平倉、只剩一腿有部位時也適用", // the leftover-leg case
            "提前出場也會暫停，排程出場照常", // what the stop does to exits
            "不會開新倉、加碼、補另一腿", "配對以外的持倉", "不會改變配對狀態", "仍需你確認已平倉", // what it never does
            "查證結果不明或送單用完時不會動作", "停止開新倉", NOTE, // when unsure
        ] {
            assert!(t.contains(needle), "missing {needle:?} in {t}");
        }
        assert!(!t.contains("通知你") && !t.contains("重試"), "no promise of a notification that is not wired, no \"retry\": {t}");
    }

    #[test]
    fn a_timeout_override_gets_a_note_and_no_override_gets_none() {
        assert_eq!(rules_override_note(&[]), None);
        let note = rules_override_note(&[(Exchange::Bybit, 7), (Exchange::Binance, 20)]).unwrap();
        assert!(note.contains("Binance 20 秒") && note.contains("Bybit 7 秒") && note.contains("較小"), "{note}");
    }

    // ---- 3.3 the manual-handling card ----

    /// 2027-01-15 08:00:00 UTC.
    const NOW: i64 = 1_800_000_000_000;
    const PAIR: (Exchange, Exchange) = (Exchange::Binance, Exchange::Bybit);

    #[test]
    fn waiting_says_when_the_check_comes() {
        let lines = status_lines(&Status::Waiting { due_ms: Some(NOW + 5_000) }, PAIR, NOW);
        assert_eq!(lines, vec!["看門狗等待中：將在 08:00:05 UTC 查證（訂單逾時後再等 5 秒）；只有確認只有一腿成交時才會自動平掉該腿。".to_string()]);
        assert_eq!(status_lines(&Status::Waiting { due_ms: Some(NOW - 1) }, PAIR, NOW), vec!["看門狗查證中…".to_string()], "the time has come");
        let unknown_time = status_lines(&Status::Waiting { due_ms: None }, PAIR, NOW);
        assert!(unknown_time[0].contains("再等 5 秒") && !unknown_time[0].contains("UTC"), "{unknown_time:?}");
    }

    #[test]
    fn a_flatten_that_happened_names_the_leg_the_quantity_and_what_the_user_does_next() {
        let lines = status_lines(&Status::Flattened { leg: Leg::Long, quantity: dec("10") }, PAIR, NOW);
        assert_eq!(lines[0], "系統已自動平掉多腿，請確認兩所皆無部位後按「人工確認已平倉」。", "the spec's sentence");
        assert_eq!(lines[1], "明細：Binance 賣出 10（只減倉市價單）。");
        let short = status_lines(&Status::Flattened { leg: Leg::Short, quantity: dec("0.5") }, PAIR, NOW);
        assert_eq!((short[0].as_str(), short[1].as_str()), ("系統已自動平掉空腿，請確認兩所皆無部位後按「人工確認已平倉」。", "明細：Bybit 買進 0.5（只減倉市價單）。"));
    }

    #[test]
    fn hedged_and_flat_ask_for_a_manual_decision_and_say_nothing_was_sent() {
        assert_eq!(
            status_lines(&Status::Hedged { long: Some(dec("10")), short: Some(dec("7")) }, PAIR, NOW),
            vec!["兩腿都有部位（可能不平衡）：多腿 10、空腿 7，系統不會送單，請人工處理。".to_string()],
            "both quantities are listed"
        );
        let unknown_sizes = status_lines(&Status::Hedged { long: None, short: None }, PAIR, NOW);
        assert_eq!(unknown_sizes, vec!["兩腿都有部位（可能不平衡），系統不會送單，請人工處理。".to_string()]);
        assert!(!unknown_sizes[0].contains("已對沖"), "{unknown_sizes:?}");
        assert_eq!(status_lines(&Status::Flat, PAIR, NOW), vec!["兩腿都沒有部位，系統不會送單，請人工處理；確認兩所皆無部位後按「人工確認已平倉」。".to_string()]);
    }

    #[test]
    fn unknown_gives_the_reason_and_says_new_entries_are_stopped() {
        let lines = status_lines(&Status::Unknown { reason: "long position unreadable: timeout".into(), sent: vec![] }, PAIR, NOW);
        assert_eq!(lines, vec![unknown_sentence("long position unreadable: timeout", &[])]);
        assert!(lines[0].contains("long position unreadable: timeout") && lines[0].contains("停止開新倉") && lines[0].contains("人工處理"));
    }

    #[test]
    fn the_in_between_states_say_what_is_going_on_and_when_the_next_look_is() {
        let cancel = status_lines(&Status::WaitingCancel { waited: 2, next_ms: NOW + 5_000 }, PAIR, NOW);
        assert!(cancel[0].contains("尚未終結") && cancel[0].contains("撤單") && cancel[0].contains("08:00:05 UTC") && cancel[0].contains("第 2 輪"), "{cancel:?}");
        assert!(cancel[0].contains(&format!("累計 {MAX_CANCEL_WAIT_ROUNDS} 輪")), "{cancel:?}");
        let flattening = status_lines(&Status::Flattening { leg: Leg::Long, quantity: dec("10"), attempt: 2 }, PAIR, NOW);
        assert!(flattening[0].contains("多腿") && flattening[0].contains("只減倉") && flattening[0].contains("數量 10") && flattening[0].contains("第 2 次") && flattening[0].contains("最多送出 3 次"), "{flattening:?}");
        let retry = status_lines(&Status::Retrying { attempt: 1, reason: "order rejected: risk limit".into(), next_ms: NOW + 5_000 }, PAIR, NOW);
        assert!(retry[0].contains("第 1 次未成功") && retry[0].contains("order rejected: risk limit") && retry[0].contains("08:00:05 UTC") && retry[0].contains("最多送出 3 次"), "{retry:?}");
        assert!(retry[0].contains(NOTE) && !retry[0].contains("通知你"), "{retry:?}");
        let read = status_lines(&Status::ReadRetry { failed: 2, reason: "long position unreadable: timeout".into(), next_ms: NOW + 5_000 }, PAIR, NOW);
        assert!(read[0].contains("讀取失敗") && read[0].contains("long position unreadable: timeout") && read[0].contains("08:00:05 UTC") && read[0].contains("第 2 輪"), "{read:?}");
        assert!(read[0].contains("累計 3 輪") && read[0].contains("停止開新倉"), "{read:?}");
        let skipped = status_lines(&Status::Skipped { reason: "pair belongs to EXCHANGE_DEMO".into() }, PAIR, NOW);
        assert!(skipped[0].contains("不處理") && skipped[0].contains("EXCHANGE_DEMO"), "{skipped:?}");
    }

    #[test]
    fn no_line_promises_or_hides_anything_the_rules_do_not_say() {
        // every status mentions neither a repair of the missing leg nor an automatic state change
        let all = [
            Status::Waiting { due_ms: Some(NOW) },
            Status::WaitingCancel { waited: 1, next_ms: NOW },
            Status::Flattening { leg: Leg::Short, quantity: dec("1"), attempt: 1 },
            Status::Retrying { attempt: 1, reason: "x".into(), next_ms: NOW },
            Status::Flattened { leg: Leg::Short, quantity: dec("1") },
            Status::Hedged { long: Some(dec("1")), short: Some(dec("1")) },
            Status::Flat,
            Status::ReadRetry { failed: 1, reason: "x".into(), next_ms: NOW },
            Status::Unknown { reason: "x".into(), sent: vec![] },
            Status::Skipped { reason: "x".into() },
        ];
        for st in all {
            let text = status_lines(&st, PAIR, NOW).join("\n");
            assert!(!text.is_empty(), "{st:?}");
            assert!(!text.contains("補另一腿已") && !text.contains("已回滾") && !text.contains("已補"), "{st:?}: {text}");
        }
    }

    #[test]
    fn the_card_intro_names_the_rules_block_and_promises_no_repair() {
        assert!(CARD_INTRO.contains(RULES_TITLE), "{CARD_INTRO}");
        assert!(CARD_INTRO.contains("不會補買、補賣") && CARD_INTRO.contains("不會改變配對狀態"), "{CARD_INTRO}");
        assert!(!CARD_INTRO.contains("不會自動補救"), "the old sentence is false now that the watchdog can flatten a leg");
    }

    #[test]
    fn the_pair_alert_note_states_the_rule_in_one_line() {
        let note = pair_alert_note();
        assert!(note.contains("不會補單") && note.contains("只有確認只有一腿成交") && note.contains("再等 5 秒") && note.contains("風控設定頁"), "{note}");
        assert!(!note.contains('\n'), "one line");
    }
}

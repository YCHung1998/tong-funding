//! Arming panel view-model (live-guardrails 4.2). Real-money screen: every sentence and every
//! button-enabling decision is made here (pure, testable); the page only draws and forwards.
//! The panel exists for the Live identity only (`evaluate` returns `None` for Demo).
//!
//! The engine is the authority: it re-verifies the preflight id, the caps hash, the confirmation
//! sentence and the checklist flag on `Arm`. This view-model only refuses to *offer* the button
//! until the same conditions hold on this side, and never loosens them.

use tong_funding_core::types::Decimal;

use super::bridge::{CommandSink, KillSwitchState, UiSnapshot};
use crate::engine::command::Command;
use crate::engine::live_guard::{self, ARM_CONFIRMATION, PREFLIGHT_VALID_MS};
use crate::engine::preflight::{self, CheckOutcome, PreflightReport};

/// The manual checklist the operator ticks (all must be ticked for `checklist_ack`).
pub const CHECKLIST: [&str; 4] = [
    "已在交易所關閉 API 金鑰的提領權限",
    "已在交易所為 API 金鑰設定 IP 白名單",
    "使用專用子帳戶，且只存入小額資金",
    "已在交易所頁面核對手續費率",
];

/// What the operator typed and ticked (page state).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArmingForm {
    pub checked: [bool; CHECKLIST.len()],
    pub confirmation: String,
}

impl ArmingForm {
    pub fn checklist_complete(&self) -> bool {
        self.checked.iter().all(|c| *c)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightLine {
    pub text: String,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArmingVm {
    pub armed: bool,
    pub status_text: String,
    /// Words about the preflight as a whole (not run / running / stale / failed / passed).
    pub preflight_summary: String,
    pub preflight_lines: Vec<PreflightLine>,
    pub caps_title: &'static str,
    pub caps_lines: Vec<String>,
    pub checklist: [&'static str; CHECKLIST.len()],
    pub confirmation_phrase: &'static str,
    pub can_run_preflight: bool,
    pub can_arm: bool,
    /// Why "武裝" is disabled (empty when it is enabled).
    pub arm_blockers: Vec<String>,
    pub can_lock: bool,
}

fn check_label(name: &str) -> &str {
    match name {
        preflight::CHECK_KEYS => "兩所金鑰皆已存在",
        preflight::CHECK_ACCOUNT_READ => "帳戶餘額與持倉可讀取",
        preflight::CHECK_CLOCK => "時鐘已校時",
        preflight::CHECK_ONE_WAY => "單向持倉模式",
        preflight::CHECK_NO_EXPOSURE => "無既有部位與掛單",
        preflight::CHECK_MARGIN => "可用保證金足夠",
        preflight::CHECK_SETTINGS => "上限與設定已填寫且在天花板內",
        preflight::CHECK_DB => "資料庫可寫入",
        preflight::CHECK_DISK => "磁碟剩餘空間足夠",
        other => other,
    }
}

fn lines_of(report: &PreflightReport) -> Vec<PreflightLine> {
    report
        .items
        .iter()
        .map(|i| {
            let ex = i.exchange.map(|e| format!("［{}］", e.name())).unwrap_or_default();
            match &i.outcome {
                CheckOutcome::Pass => PreflightLine { text: format!("通過　{}{ex}", check_label(i.name)), pass: true },
                CheckOutcome::Fail(why) => PreflightLine { text: format!("失敗　{}{ex}：{why}", check_label(i.name)), pass: false },
            }
        })
        .collect()
}

fn dec(d: Decimal) -> String {
    d.normalize().to_string()
}

fn caps_lines(snap: &UiSnapshot) -> Vec<String> {
    let risk = &snap.settings.risk;
    let (leg, pairs, concurrent, leverage, note) = match &snap.live.frozen {
        Some(f) => (Some(f.leg_notional_usdt), Some(f.pairs_per_arming), f.max_concurrent_pairs, f.max_leverage, "（已凍結：武裝期間只能調低、不能調高）"),
        None => (risk.live_max_leg_notional_usdt, risk.live_max_pairs_per_arming, risk.max_concurrent_pairs, risk.max_leverage, "（目前設定值；武裝時凍結）"),
    };
    vec![
        format!("每腿名目上限：{}", leg.map_or("未設定".to_string(), |d| format!("{} USDT", dec(d)))),
        format!("每次武裝可進場筆數：{}", pairs.map_or("未設定".to_string(), |n| n.to_string())),
        format!("槓桿上限：{}x", dec(leverage)),
        format!("同時配對數上限：{concurrent}"),
        note.to_string(),
    ]
}

/// `None` unless the process identity is Live (the panel never appears in Demo).
pub fn evaluate(snap: &UiSnapshot, form: &ArmingForm, now_ms: i64) -> Option<ArmingVm> {
    if !snap.identity.is_live() {
        return None;
    }
    let live = &snap.live;
    let armed = live.armed;
    let engine_up = snap.engine.is_some();

    let status_text = if armed {
        let cap = live.frozen.as_ref().map_or("?".to_string(), |f| f.pairs_per_arming.to_string());
        format!("已武裝：本次已進場 {} / 上限 {cap} 筆", live.pairs_entered)
    } else {
        match &live.last_lock_reason {
            Some(r) => format!("已鎖定（只能平倉、取消）：{r}"),
            None => "已鎖定（只能平倉、取消）：需完成預檢與確認才能武裝".to_string(),
        }
    };

    let report = live.preflight.as_ref();
    let own_hash = live_guard::caps_hash(&snap.settings.risk);
    let fresh = |r: &PreflightReport| -> Result<(), String> {
        let age = now_ms.saturating_sub(r.issued_at_ms);
        if (0..=PREFLIGHT_VALID_MS).contains(&age) { Ok(()) } else { Err(format!("預檢已過期（{} 秒前；有效 {} 秒），請重新執行預檢", age / 1000, PREFLIGHT_VALID_MS / 1000)) }
    };
    let preflight_summary = if live.preflight_running {
        "預檢執行中…".to_string()
    } else {
        match report {
            None => "尚未執行預檢".to_string(),
            Some(r) if !r.passed() => format!("預檢未通過：{} 項失敗", r.items.iter().filter(|i| i.outcome != CheckOutcome::Pass).count()),
            Some(r) => match fresh(r) {
                Ok(()) => format!("預檢全數通過（{} 秒內有效）", PREFLIGHT_VALID_MS / 1000),
                Err(e) => e,
            },
        }
    };

    let mut blockers = Vec::new();
    if !engine_up {
        blockers.push("引擎未啟動".to_string());
    }
    if armed {
        blockers.push("已武裝".to_string());
    }
    if live.preflight_running {
        blockers.push("預檢執行中".to_string());
    }
    match report {
        None => blockers.push("尚未執行預檢".to_string()),
        Some(r) => {
            if !r.passed() || r.id.is_none() {
                blockers.push("預檢未通過".to_string());
            } else if let Err(e) = fresh(r) {
                blockers.push(e);
            }
            match (&r.caps_hash, &own_hash) {
                (Some(a), Some(b)) if a == b => {}
                _ => blockers.push("預檢之後上限已變動或未設定，請重新執行預檢".to_string()),
            }
        }
    }
    if !form.checklist_complete() {
        blockers.push("人工清單尚未全部勾選".to_string());
    }
    if !live_guard::confirmation_matches(&form.confirmation) {
        blockers.push(format!("確認字串須完全輸入「{ARM_CONFIRMATION}」"));
    }
    let can_arm = blockers.is_empty();

    Some(ArmingVm {
        armed,
        status_text,
        preflight_summary,
        preflight_lines: report.map(lines_of).unwrap_or_default(),
        caps_title: "本次武裝的硬上限",
        caps_lines: caps_lines(snap),
        checklist: CHECKLIST,
        confirmation_phrase: ARM_CONFIRMATION,
        can_run_preflight: engine_up && !live.preflight_running && !armed,
        can_arm,
        arm_blockers: blockers,
        can_lock: engine_up && (armed || snap.system.kill_switch == KillSwitchState::Off),
    })
}

/// Sends `RunPreflight`. `Err` = refused here (never reaches the engine).
pub fn run_preflight(vm: &ArmingVm, sink: &dyn CommandSink) -> Result<(), String> {
    if !vm.can_run_preflight {
        return Err("目前無法執行預檢".into());
    }
    sink.run_preflight("執行預檢".into());
    Ok(())
}

/// Sends `Arm` with the engine's own preflight id and caps hash. Re-evaluates the conditions
/// from the same inputs, so a stale `vm` can never arm.
pub fn arm(snap: &UiSnapshot, form: &ArmingForm, now_ms: i64, sink: &dyn CommandSink) -> Result<(), String> {
    let vm = evaluate(snap, form, now_ms).ok_or("此程式不是真錢身分，沒有武裝功能")?;
    if !vm.can_arm {
        return Err(vm.arm_blockers.join("；"));
    }
    let report = snap.live.preflight.as_ref().ok_or("尚未執行預檢")?;
    let (Some(id), Some(hash)) = (report.id.clone(), report.caps_hash.clone()) else {
        return Err("預檢沒有可用的識別碼".into());
    };
    sink.arm("武裝".into(), id, hash, form.confirmation.clone(), form.checklist_complete());
    Ok(())
}

/// Sends the existing kill-switch command (lock now).
pub fn lock_now(vm: &ArmingVm, sink: &dyn CommandSink) -> Result<(), String> {
    if !vm.can_lock {
        return Err("目前已鎖定".into());
    }
    sink.send("立即鎖定".into(), Command::SetKillSwitch { on: true });
    Ok(())
}

#[cfg(test)]
#[path = "arming_tests.rs"]
mod tests;

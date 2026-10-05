//! Sidebar pages: fixed order, bilingual labels, and the debug-tool separation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Scanner,
    ContractSettings,
    StagedOrders,
    Positions,
    RiskSettings,
    SystemLogs,
    ManualOrder,
}

impl Page {
    /// Display order. `ManualOrder` is last and rendered below a divider.
    pub const ALL: [Page; 8] = [
        Page::Overview,
        Page::Scanner,
        Page::ContractSettings,
        Page::StagedOrders,
        Page::Positions,
        Page::RiskSettings,
        Page::SystemLogs,
        Page::ManualOrder,
    ];

    pub fn zh(self) -> &'static str {
        match self {
            Page::Overview => "總覽",
            Page::Scanner => "掃幣",
            Page::ContractSettings => "合約設定",
            Page::StagedOrders => "交易單",
            Page::Positions => "持倉",
            Page::RiskSettings => "風控設定",
            Page::SystemLogs => "系統日誌",
            Page::ManualOrder => "手動下單",
        }
    }

    pub fn en(self) -> &'static str {
        match self {
            Page::Overview => "Dashboard",
            Page::Scanner => "Full-Market Scanner",
            Page::ContractSettings => "Contract Settings",
            Page::StagedOrders => "Staged Orders",
            Page::Positions => "Unified Positions",
            Page::RiskSettings => "Risk Management",
            Page::SystemLogs => "System Logs",
            Page::ManualOrder => "Debug Tool",
        }
    }

    pub fn is_debug(self) -> bool {
        matches!(self, Page::ManualOrder)
    }

    pub fn default_page() -> Page {
        Page::Overview
    }
}

pub const DEBUG_WARNING: &str = "除錯工具，非標準流程";

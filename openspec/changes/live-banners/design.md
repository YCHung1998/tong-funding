## Context

- 畫面是 GPUI；頁面 view model 在 `app/src/ui/vm/*`（純函式、可測），畫面在 `app/src/ui/*.rs`。狀態列與模式徽章在 `app/src/ui/status.rs`（目前 `ExecutionMode` 只有 `Simulation`／`ExchangeDemo`）。
- 環境文字散落多處且用布林或固定字串：`app/src/engine/alert.rs:143`（通知把非模擬寫成 `EXCHANGE_DEMO`）、`app/src/ui/vm/manual_order.rs:396-398`（非 Demo 時顯示模擬帳本）、`app/src/ui/trading_pages.rs`（約 450、720、840、892、910-915 行：觸發模式說明、配對卡 `if h.simulated { "SIMULATION" } else { "EXCHANGE_DEMO" }`、模式按鈕啟用條件、模式切換確認文字「將對 demo / testnet 帳戶真實下單（不涉及真錢）」）。
- `app-shell` 規格目前要求「不出現 LIVE 字樣」；`live-process` 提供程式身分 `ProcessIdentity { Demo, Live }`。

## Goals / Non-Goals

**Goals:**
- 看一眼就知道現在是 LIVE、DEMO 還是模擬；每個花錢動作的確認都重複環境與金額。
- 所有環境文案來自同一個窮舉函式，新增身分時編譯器會指出每個要更新的位置。

**Non-Goals:**
- 武裝流程、硬上限、自動 kill switch（`live-guardrails`）；macOS 系統通知；整個 app 的換色主題；輸入框不可貼上。
- 不改 Demo 與模擬的既有行為（除了把硬寫字串改為由同一函式產生，文字不變）。

## Decisions

1. **單一來源 `EnvLabel`**：新增 `ui/vm/env_label.rs`，輸入 `(ProcessIdentity, ExecutionMode)`，輸出橫幅文字、色調、視窗標題前綴、徽章文字、確認按鈕文字；所有畫面與通知都呼叫它，使用窮舉 `match`（不用 `_`）。測試涵蓋 2×2 組合。替代方案：各頁自己寫字串——被否決（這正是現況的問題）。
2. **橫幅放在 `Shell` 的頁面框架最上層**，所有頁面共用；沒有任何事件處理器可以隱藏它（用測試確認 vm 沒有「隱藏」狀態欄位）。顏色用既有的 `theme` 色票（NEGATIVE／WARNING／靜音），文字一定存在。
3. **視窗標題**由 `gpui_kit::open_window` 的選項設定；若框架不支援執行期更新，啟動時依身分設一次即可（身分不變）。
4. **確認對話框**：把現有各處確認的文字函式化，加入 `EnvLabel` 與名目；Live 的按鈕文字為「以真錢下單」／「以真錢平倉」。金額來自既有 vm（合約模板名目、配對名目）。
5. **`engine/alert.rs` 的環境文字**：引擎本身不知道程式身分（身分在組合根）；以建構參數傳入一個 `EnvName`（`Live`／`Demo`／`Sim`），通知文字由它產生。替代方案：讓引擎讀全域身分——被否決（避免全域狀態）。
6. **手動下單頁帳本來源**：`account_is_simulated` 改為依 `(身分, 模式)` 回傳帳本來源列舉並窮舉。
7. **`app-shell` 規格**以 MODIFIED 取代原請求，保留原有的首次啟動預設與 Demo 標示情境，並新增 Live 情境。

## Risks / Trade-offs

- [橫幅佔用畫面空間] → 單行高度、固定在頂端；只在狀態需要時多顯示 kill switch 文字，不另外增加行數。
- [GPUI 的視窗標題或文字測試能力有限] → 把文字與色調的決定都放在 vm（可測），畫面只負責渲染；煙霧測試確認不 panic。
- [文案分散導致遺漏] → 以 grep 清單列出所有 `EXCHANGE_DEMO`／`SIMULATION` 的顯示位置（tasks 1.1），並加測試掃描 `ui/` 內不得再出現硬寫的環境字串。
- [Live 的 kill switch 顯示要等 `live-guardrails`] → 本變更先顯示 kill switch 現況（開／關）；「已武裝」的語意由 `live-guardrails` 定義後再對齊。

## Migration Plan

1. 先寫 `EnvLabel` 與它的測試，再替換各處文案，最後加橫幅與標題。
2. 無資料與設定變更，部署即生效；回滾即還原 commit。

## Open Questions

- Live 的紅色是否需要和現有的負向色（虧損紅）區分，以免與損益紅色混淆？（預設使用同一個紅，靠文字與固定位置區分；若你覺得會混淆再調整。）

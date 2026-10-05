## Context

**已驗證的事實**

| 事實 | 來源 |
|---|---|
| Python 版手動下單頁**不受** `execution_mode` 影響，一律真的送單 | `mvp-python/app.py:730`（FAQ 文字）、`app.py:1682-1685`（風控頁 help 文字） |
| Python 版 `trigger_mode` 切換放在交易單頁（「自動排程」radio），MANUAL 時才顯示「立即執行」（`PREPARED`）與「立即平倉」（`RECONCILED`），AUTO 時顯示「（自動）」 | `app.py:1159`、`app.py:1216-1226` |
| Python 版風控預設值：`max_leverage` 5、`max_concurrent_pairs` 3、`order_timeout_seconds` 15、`max_leg_imbalance_pct` 1.0、`min_24h_volume_usdt` 50000、`stale_data_threshold_ms` 1000、`execution_mode` SIMULATION、`trigger_mode` AUTO | `mvp-python/risk_config.py` |
| Python 版另有 `min_expected_net_pnl_pct`（預設 0.03）；Figma 風控頁的「Min Expected Net PnL %」即此欄 | `risk_config.py`、Figma 風控設定畫面 |
| Python 版合約模板預設為 Notional 1000、Leverage 5；連動公式為 `notional ÷ leverage`、`notional ÷ margin` | `mvp-python/contract_settings.py` |
| Python 版覆寫只存不讀（HANDOFF Fragility #1），且 UI 每個交易所只暴露槓桿與滑價 | `HANDOFF.md`、`risk_config.py` |
| Figma 風控頁的 Allowed Exchanges 只有 Binance、Bybit；沒有 `min_24h_volume_usdt` 欄位 | Figma 風控設定畫面 |
| Figma 手動下單頁只有 Binance 與 Bybit 兩個面板，且文字寫死「SIMULATION 已啟用 · 無 API Key / 無真實帳戶 / 無外部請求」 | Figma 手動下單畫面 |
| Figma 交易單頁「上次模擬執行結果」寫著「對腿失敗後已平倉回滾」，與「完全人工」決策衝突 | Figma 交易單畫面 |
| Figma 持倉、合約設定、交易單頁數字（1,200 / 3× / 0.019934 BTC / Paper cash 30,410）皆為示範值 | Figma 各畫面末行「靜態示範資料」 |

`core` 與 `store` 已提供的規則（本 change 直接呼叫，不重新定義）：`risk-config`（欄位、預設、驗證、`effective_for_pair`）、`net-edge`、`quantity-precision`、`pretrade-validation`、`pair-lifecycle`（含人工事件）、事件表與設定持久化。

色票與字型代幣使用 `bootstrap-gpui-shell` 的 theme 模組，本 change 不重寫色碼。

## Goals / Non-Goals

**Goals**
- 讓使用者只能以「看得見每一腿、確認過」的方式送出訂單。
- 設定不完整時，不是靜默用 0 或預設值，而是明確禁止並說明。
- 各所覆寫、模式單選、手動下單的約束都有測試證明確實影響執行路徑，而不是只存檔。
- 單腿失敗只有人工處理入口，頁面沒有任何自動補救的動作。

**Non-Goals**
- 不做掃幣、總覽、持倉、系統日誌（`ui-readonly-pages`）。
- 不做全頁警示橫幅與 macOS 通知本身（`ui-readonly-pages` 的 `alert-banner`、`exchange-demo-execution` 的 `partial-failure-alerting`）；本 change 只負責橫幅指向的人工處理入口。
- 不做 Funding / PnL 顯示（`funding-pnl`）。
- 不做 OKX 下單；不做真錢；不提供 LIVE 模式。

## Decisions

**D1　UI 邏輯放在不依賴 GPUI 型別的 view-model 純函式。**
連動計算、數量試算、確認清單、禁用判定都是會出錯且可窮舉的邏輯，應能以 `cargo test -p app` 驗證；GPUI 只負責繪製。
理由：沿用 `bootstrap-gpui-shell` D1 的邊界精神。代價：多一層對應，但換到可測性。

**D2　頁面不持有任何交易所 client，只送 engine Command。**
手動下單頁、交易單頁一律以 Command 與 engine 溝通，才能做到「系統只有一條下單路徑」，`execution_mode`、kill switch、停機狀態、`opens_exposure` 都在 engine 一處把關。
驗收：以依賴檢查證明 UI 模組不依賴 exchange client（task 4.1）。

**D3　手動下單頁改為受 `execution_mode` 約束（與 Python 版刻意不同）。**
Python 版的手動下單永遠真送單，使「SIMULATION 很安全」這個承諾有漏洞。新版 `SIMULATION` 下手動下單禁用，要真的下單必須先明確切到 `EXCHANGE_DEMO`。
副作用：想在 `SIMULATION` 下手動平掉遺留倉位，必須先切模式（列入 Open Questions）。

**D4　送出前的二次確認清單由純函式產生，且與送給 engine 的命令使用同一份資料。**
避免「畫面列的」與「實際送的」不一致：確認視窗的每一腿與 Command 內容由同一個結構產生，測試以腿數與內容相等性驗證。

**D5　禁用原因是列舉型別，不是自由字串。**
`NothingSelected`、`ConfigIncomplete(缺漏欄位清單)`、`KillSwitchOn`、`Halted`、`NotPrepared`。窮舉才能保證每個原因都有測試與對應文案，新增原因時編譯器會提醒。

**D6　「設定不完整」只有一個來源：`risk-config` 的完整性判定。**
頁面不重寫判定；風控頁顯示缺漏清單，交易單頁引用同一結果。避免兩頁各自判定而不一致。

**D7　`trigger_mode` 切換放在交易單頁（沿用 Python 版）。**
Figma 沒有這個控制。放這裡是因為它直接影響此頁的「立即平倉」與排程行為。已列入 Open Questions 請使用者確認。

**D8　一鍵送出在 AUTO 與 MANUAL 下皆可用。**
Python 版只在 MANUAL 顯示「立即執行」。Figma 的一鍵送出沒有模式限制。AUTO 下重複送出由 engine 的原子狀態轉移（`PREPARED` 只能被轉出一次）防止，頁面不另做互斥。列入 Open Questions。

**D9　`taker_fee_pct` 對三個交易所皆為必填，包含只做公開行情的 OKX。**
與 `risk-config` spec 一致（「每個交易所各一」）。理由：掃幣頁的 Net Edge 會涵蓋含 OKX 的配對。

## 與 Figma / Python 版的差異清單

| 項目 | Figma 或 Python 版 | 本 change | 原因 |
|---|---|---|---|
| 風控：Funding Threshold、Max Concurrent Trades（legs）、Hedge Threshold | Figma 有 | 移除 | 使用者拍板；與 `risk-config` 一致 |
| 風控：Min Expected Net PnL % | Figma 有 | 由 `net_edge_threshold_pct` 取代 | 達標改由 Net Edge 決定（需使用者確認為取代關係） |
| 風控：Max Slippage % | 單一欄位 | 拆成「最大價格漂移」與「估計滑價」 | `risk-config` D3 |
| 風控：Stale Data Threshold | 5 sec | 1000 ms | 使用者拍板 |
| 風控：Order Timeout | 1,500 ms | 15 秒 | 使用者拍板 |
| 風控：模式 | SIMULATION / LIVE | SIMULATION / EXCHANGE_DEMO，無 LIVE 警告 | 不做真錢 |
| 風控：Net Edge 欄位與各所 taker 費率 | 無 | 新增 | `net-edge` |
| 風控：`min_24h_volume_usdt` | 無 | 新增欄位 | `risk-config` 有此欄位，Figma 漏畫 |
| 風控：覆寫欄位 | 槓桿、滑價兩項 | 九個可覆寫欄位，且接進執行路徑 | 使用者保留覆寫功能 |
| 風控：預檢摘要「Spread − 0.020% 費用預留 − 0.005% 滑價預留 ≥ 0.005%」 | 寫死數字 | 依現值呈現 Net Edge 公式，缺值顯示「未設定」 | 不得有憑空數字 |
| 交易單：一鍵送出 | 直接送 | 先逐腿列出並二次確認 | 使用者拍板 |
| 交易單：「上次模擬執行結果」 | 僅模擬、含「已回滾」 | 涵蓋兩種模式、標示模式、單腿失敗顯示「需人工處理」 | 完全人工決策 |
| 交易單：「Paper cash 可用」 | 虛構現金 | 交易所帳戶實際可用保證金，查不到顯示「未知」 | 無 Paper 帳戶概念 |
| 交易單：人工處理入口、`trigger_mode`、立即平倉 | Figma 無 | 新增 | 完全人工決策；Python 版既有功能 |
| 合約設定：預期 Quantity 0.019934（六位小數） | 未取整 | 顯示依 lot size 向下取整後的數量與低於最小量提示 | `quantity-precision` |
| 合約設定：OKX | 無 | 數量以合約張數顯示 | 使用者拍板 |
| 手動下單：受模式約束 | Python 版不受約束 | 受約束，`SIMULATION` 下禁用 | 使用者拍板 |
| 手動下單：環境說明 | 寫死「無外部請求」 | 依模式如實顯示 | 避免不實說明 |

## Risks / Trade-offs

- **GPUI 的對話框（確認視窗）元件尚未驗證。** `bootstrap-gpui-shell` 只確認了表格、圖表、側邊欄、標題列與狀態列元件；modal / dialog 是否現成**未驗證**。若沒有，需自行以疊層實作，應在 task 1.2 一開始確認，必要時調整估時。
- **二次確認會增加送出的摩擦。** 這是使用者要求的取捨。
- **覆寫的整合測試依賴 engine 的送單前檢查已呼叫 `effective_for_pair`。** 若 `engine-simulation` 尚未完成，task 3.3 的整合測試無法轉綠；本 change 在其後實作。
- **Python demo 運行中 `PAIR_PARTIAL_FAILURE` 有 47 筆、`PAIR_FINALIZED` 有 48 筆**（`store-sqlite` 設計文件所載，原因未查證）。若新版也頻繁單腿失敗，人工處理入口會被高頻使用，需要在 task 5.1 實測並評估入口是否夠用。
- **槓桿是否必須為整數未驗證。** 「用保證金反推槓桿」可能得到非整數（例如 1,200 ÷ 350）。Binance 設定槓桿的參數型別、Bybit 是否接受小數**未驗證**；本 change 的 spec 只要求大於 0，送單前由 engine／交易所設定槓桿的步驟處理。

## Open Questions

1. **（與 Figma／既有欄位的關係，需人類確認）** Figma「Min Expected Net PnL %」是否確定由 `net_edge_threshold_pct` 取代？Python 版 `min_expected_net_pnl_pct`（預設 0.03）在 `core` 的 `risk-config` 中既未保留也未列入「移除」清單，屬於隱性取代。
2. **（與 Figma 差異）** `trigger_mode` 切換放在交易單頁（D7）、`min_24h_volume_usdt` 新增於風控頁，是否接受？
3. **（行為變更）** 手動下單改為受 `execution_mode` 約束（D3），代價是 `SIMULATION` 下無法手動平倉遺留倉位；kill switch 啟動時手動平倉也會被擋（engine 把手動單視為增加曝險，因為頁面沒有 reduce-only 選項，且 `opens_exposure` 的分類屬 engine 設計）。是否需要為手動單加 reduce-only 並讓其在 kill switch 下仍可送出？
4. **（D8）** AUTO 模式下是否允許一鍵送出？Python 版只在 MANUAL 提供。
5. **槓桿是否必須為整數？** 見 Risks。
6. **可用保證金的來源。** 本設計假設交易單頁顯示的是 `exchange-readonly-adapters` 讀到的 demo 帳戶可用保證金；`SIMULATION` 下沒有任何 Paper 帳戶。若使用者仍想要 Paper cash 概念，需要另開設計。
7. **`taker_fee_pct` 對 OKX 是否必填**（D9）：若 OKX 不在 `allowed_exchanges`，是否仍要求填寫？目前依 `risk-config` spec 一律要求。
8. **預設值的取捨。** 合約模板預設 1000 / 5（Python 版）而非 Figma 的 1,200 / 3×，Figma 值視為示範。

## 未驗證清單

- GPUI 是否有現成的 modal / dialog 元件（Risks）。
- 交易所是否接受非整數槓桿（Risks）。
- 各所 `step_size`、`min_qty`、`ct_val` 的實際數值：本 change 不寫死任何數字，由 `exchange-readonly-adapters` 提供。
- 取得 demo 金鑰的 Keychain 行為（`store-sqlite` 的 Open Questions）。

## 決定紀錄（2026-10-05，使用者）

- 掃幣頁的「加入交易單」欄與 Candidate List 由本 change 承接（原本不屬於任何 change）：需求待撰寫，列入 task 1.x 的擴充；實作前先補 spec。

## 決定紀錄（2026-10-05 晚，使用者）

- **AUTO 模式也提供一鍵送出**（Open Question 4），仍需二次確認；送出前檢查該配對沒有被排程器觸發中，避免重複進場。
- **「Min Expected Net PnL %」與 `net_edge_threshold_pct` 兩個都保留**（Open Question 1）：需在 `core` 的 `risk-config` 新增 `min_expected_net_pnl_pct` 並納入送單前檢查（作為本 change 對 `risk-config` / `pretrade-validation` 的 MODIFIED）。
- 手動下單已有 `reduce_only`，kill switch 啟動時 reduce-only 手動單仍可送出（`engine-simulation` D5，Open Question 3 已解）。

## Why

`live-process` 讓程式能以 `--live` 身分對 Binance／Bybit 正式環境下真單，`live-banners` 讓使用者一眼看出環境。但真錢還需要**在引擎裡**（不是只在畫面上）強制執行的保護：預設不能下單、要經過明確的武裝、單腿金額與筆數有硬上限、出事時自動鎖住只剩平倉，以及手動下單頁不能成為繞過上限的出口。

2026-10-15 的三鏡頭審查指出現況的缺口：合約模板名目只檢查大於 0、手動下單頁完全不受上限、武裝確認只存在畫面層、kill switch 不會自動打開、設定可在武裝後被放寬。使用者拍板階段 2 的數字：每腿 10 USDT、每所存 30 USDT、同時 1 個配對、一次武裝最多 2 筆、槓桿 5 倍。

## What Changes

- **Live 啟動時 kill switch 強制為 ON**（每次啟動，包含當機後重啟）；kill switch 為 ON 時只擋開倉，平倉與取消照常（沿用既有語意）。
- **武裝是引擎指令**：`Arm { preflight_id, caps_hash, confirmation, checklist_ack }`。`preflight_id` 必須是引擎自己在最近 60 秒內產生且全綠的預檢結果；`confirmation` 必須等於固定確認字串（引擎檢查，不只是畫面）；成功後 kill switch 關閉、上限凍結在引擎記憶體、本次武裝的筆數歸零，並寫 `LIVE_ARMED` 事件。
- **預檢（MVP）**：兩所 live 金鑰存在；兩所 live 唯讀餘額與持倉讀取成功（失敗即封閉，也證明不是 demo 金鑰）；兩所時鐘已校時；持倉模式為單向；帳戶沒有任何未平倉部位或掛單（專用子帳戶）；可用保證金足夠；必填上限都已設定且不超過程式內建的絕對天花板；資料庫可寫且磁碟空間足夠。其餘（提領已關閉、IP 白名單、手續費率）為人工勾選清單，`checklist_ack` 必須為真。
- **硬上限（引擎強制）**：新增 live 專用必填設定 `live_max_leg_notional_usdt`（每腿名目上限）與 `live_max_pairs_per_arming`（一次武裝最多進場配對數），並有程式內建的絕對天花板（本版：每腿 ≤ 100 USDT、每次武裝 ≤ 5 筆），設定超過天花板會被拒絕。所有非 reduce-only 的送單（含進場、補單）在 `OrderRequest` 建好之後以「數量 × 最差保護價」檢查，超過就拒絕；加入交易單、儲存合約模板、進場前檢查三處也檢查。
- **一次武裝的筆數**：進場配對累計達上限後自動 kill switch = ON。
- **Live 手動下單頁停用開倉**：引擎拒絕 live 身分下任何非 reduce-only 的手動單；保留平倉與取消；新增 `FlattenLeg` 指令（平掉某配對的單一腿，reduce-only，數量取實際持倉）處理一腿成交的情況。
- **自動 kill switch = ON 的條件**：任何配對進入 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED`、認證類錯誤、HTTP 418／429、校時失敗；每次寫 `LIVE_AUTO_LOCK` 事件（原因）。
- **設定凍結**：武裝後上限不可調高（要先 kill switch ON 再重新武裝），調低即時生效；`config set` 命令列工具不得寫入 live 資料庫的上限與觸發模式。
- **Live 資料庫初始值**：新建 live 資料庫時 `trigger_mode = MANUAL`、`early_exit_min_move_pct = 0`、合約模板名目不超過上限；必填上限未設定時無法武裝。
- **「前往交易所」連結**：配對卡與橫幅提供按鈕，用預設瀏覽器開啟該幣在 Binance 合約與 Bybit 合約的網頁交易介面（只開網址，不帶金鑰）。
- 不涵蓋（延後，開 AUTO 之前）：每日損失上限、自動對帳、手續費與金鑰權限 API、緊急平倉通道（資料庫寫入失敗時的只平倉通道）、macOS 通知、成交晚於結算的處理、IOC 限價單。

## Capabilities

### New Capabilities

- `live-guardrails`：Live 啟動鎖定、武裝指令與預檢、硬上限、筆數上限、手動單限制、自動鎖定條件、設定凍結、Live 資料庫初始值、前往交易所連結。

### Modified Capabilities

- `risk-config`：新增 live 專用欄位 `live_max_leg_notional_usdt`、`live_max_pairs_per_arming` 及其驗證與天花板。

## Impact

- 程式：`core/src/risk.rs`（欄位、天花板、驗證）、`app/src/engine/*`（`Arm`、預檢、kill switch 啟動鎖定、上限檢查、`FlattenLeg`、自動鎖定、設定凍結；需要引擎知道程式身分）、`app/src/engine/gate.rs`、`app/src/store/config_cli.rs`、`app/src/ui/vm/*` 與畫面（武裝對話框、連結按鈕；與 `live-banners` 共用頁面框架）。
- 資料：新增設定欄位與事件（`LIVE_ARMED`、`LIVE_AUTO_LOCK`），寫入既有 events 表與 config 表，無結構變更。
- 相依：依賴 `live-process`（程式身分、live 資料庫、live 客戶端）；與 `live-banners` 在畫面框架上有重疊，實作時分開檔案。

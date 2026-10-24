## 1. 白話對照（先寫失敗測試）

- [x] 1.1 `vm/labels.rs`：配對狀態（窮舉）、執行環境、觸發模式、PnL 狀態、`acct_mode_unsupported` 的中文名稱與說明；未知代碼原樣顯示；測試

## 2. 停用的交易所

- [x] 2.1 總覽：停用交易所改為一行弱化文字（可展開細節）、總資產說明只列計入的交易所；測試
- [x] 2.2 交易單可用保證金列、合約設定試算：停用交易所改為一行弱化文字；測試
- [x] 2.3 持倉頁：停用交易所的讀取錯誤改為弱化文字，但其持倉照常顯示並加註；測試

## 3. 上次執行結果

- [x] 3.1 日期、幣種（修正空白）、中文結果、被擋下原因、已完成的淨損益、訂單編號改為次要文字；測試（含 BLOCKED 與跨日排序）
- [x] 3.2 交易單、持倉、PnL 相關文字改用白話對照（狀態、模式、PnL 狀態）；測試

## 4. 驗證

- [x] 4.1 `cargo test` 全套綠燈、`cargo clippy` 無新增警告、`openspec validate ui-plain-language --strict` 通過

## 5. 審查修正（停用交易所不得隱藏真實曝險）

- [x] 5.1 失敗測試先行：停用＋`positions_incomplete`、停用＋舊資料＋最新讀取失敗、停用＋Failed／未連線／載入中＋未結束配對（引擎配對與已儲存紀錄）、已結束配對不算、停用＋讀取成功且空仍為弱化單行；總覽、持倉、交易單、警示各頁
- [x] 5.2 `vm/disabled.rs` 單一規則（`Quiet`／`Unconfirmed`／`Exposed`、未結束配對判斷、`holds_exposure`）；總覽、持倉、交易單、警示改用；`DisabledNote` 帶色調，無法確認時為警示色；註記文字改為「此交易所已停用，但仍有持倉或持倉狀態未確認」
- [x] 5.3 低嚴重度：`FINALIZED` 說明改為「已平倉並已記錄損益（可能不完整）」；掃幣表 Symbol 欄寬容得下 `1000000BABYDOGEUSDT`，1,470 px 視窗（OKX 未勾選）仍達標
- [x] 5.4 驗證：`cargo test -p tong-funding` 全綠、clippy 無新增警告、`openspec validate --all --strict`

## 6. 紅隊審查修正（ui-clarity）

- [x] 6.1 E1：`live::failed_account_state` 在「未連線」且有舊資料時保留舊資料（`Loaded { error: Some("未連線：…") }`）；已知持倉仍判有曝險、帳戶與時鐘警示照常（`alerts.rs` 把這種 `Loaded` 仍視為未連線）、總覽保留卡片；測試先行
- [x] 6.2 E2：`disabled::status` 收 `now_ms`，讀取超過 `3 × ACCOUNT_POLL_MS` 判 `Unconfirmed`；`Quiet` 的詳細文字附「N 秒前讀取」（總覽、交易單、持倉）；測試先行
- [x] 6.3 E3：`alert_accounts` 只取 `tradable_exchanges(identity)`，Live 不出現 OKX 警示；測試先行
- [x] 6.4 E4：總覽曝險摘要在有 `Unconfirmed` 時附加「（<交易所> 持倉無法確認）」並改用警示色，`excluded_note` 列出該所；測試先行
- [x] 6.5 E5：`list_pairs()` 失敗推送 `SourceUpdate::PairRecordsError`，`disabled::status` 回傳 `Unconfirmed`；測試先行
- [x] 6.6 E6：`RATE_POSITIVE` 改為蘭紫 `0xDE8FE6`（與 `WARNING` 色相差 ≥ 40°、對比 ≥ 4.5:1），`theme_tests` 加色相差斷言（同步 `scanner-layout` 規格與設計）；`bench.rs` 的 Net Edge 欄改用 `pnl_tone`
- [x] 6.7 E7：修正 `live.rs` 黏在一起的 doc 註解（`pair_entries` 與 `pair_records`）
- [x] 6.8 驗證：`cargo test -p tong-funding` 全綠、clippy 對改動無新增、`openspec validate --all --strict`

## 7. 簡潔性審查後的清理（ui-clarity，行為不變，F9 除外）

- [x] 7.1 F1：刪除只剩測試用的 `StagedVm::mode_label`、`AttemptView.mode_label`、`ContractVm.mode_label`，測試改斷言 `mode_text`
- [x] 7.2 F2：`labels::trigger_mode_text` 改用 `engine::gate::trigger_mode_str`，刪除重複的 `trigger_mode_code`
- [x] 7.3 F3：刪除死碼 `RateCell::notes()`、`Countdown::text()`，測試改斷言 `first_line()`／`second_line()`
- [x] 7.4 F4：`block_reason_label` 刪除到不了的分支（`Checks {`、`RiskLimits: live:`、`LeverageCap:`，這些一律帶 `failed_checks`）與對應測試
- [x] 7.5 F5：刪除 `DISABLED_TONE` 別名，直接用 `Tone::Muted`
- [x] 7.6 F6：`dashboard::build` 的停用折疊改為 match 前 `if fold { …; continue; }`，match 回到單一 arm
- [x] 7.7 F7：白話對照（`pair_state_plain`／`pair_state_label`／`execution_mode_label`／`trigger_mode_label`／`pnl_status_label`）只回傳中文名稱，刪除從未顯示的說明句與測試；同步本變更的 spec／design／proposal；`staged_orders` 直接用 `pair_state_plain(PairState::Blocked)`
- [x] 7.8 F8：寬度模型的輔助函式與常數（`est_text_px`、`CELL_*`）移到 `scan_view_tests.rs`；刪除同義反覆測試（`GROSS_INTERVALS_DIFFER` 字面值、列高常數比較、`needs_horizontal_scroll` 重述、寫死 hex 的 `rate_colours_are_the_decided_values`）與 `dashboard_tests` 中與 `disabled_tests` 重複的測試（合併、保留獨有斷言）
- [x] 7.9 F9：風控頁執行模式（目前模式與選項按鈕）改用 `labels::execution_mode_text`（中文為主、代碼在括號）
- [x] 7.10 驗證：`cargo test -p tong-funding` 全綠、clippy 無新增、`openspec validate --all --strict`

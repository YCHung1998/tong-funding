## 1. 決策（先寫失敗測試）

- [x] 1.1 看門狗決策純函式：輸入每腿持倉、記錄成交、委託是否終結（皆可為錯誤）與容差，輸出平倉某腿／已對沖／無部位／等待撤單／未知；測試涵蓋 spec 全部情境與 `plan_leg_close` 的方向錯、差距超出容差

## 2. 引擎

- [x] 2.1 `alert_since_ms`：轉入警示狀態時記錄；重啟時由最後一筆轉入事件還原；測試
- [x] 2.2 排程：`now ≥ alert_since_ms + 5 s` 才查證、每輪 5 秒、同配對同時只有一輪；非同步取得兩腿的持倉、逐單查證成交與終結狀態；寫 `NAKED_LEG_WATCHDOG`
- [x] 2.3 等待撤單：未終結的委託先撤，連續 3 輪未終結 → 未知
- [x] 2.4 平倉路徑抽成系統與 Live 手動共用（先落 intent、`LEG_FLATTEN` 加 `by`），看門狗在 Demo／SIMULATION／Live 皆可用；手動按鈕維持 Live 限定
- [x] 2.5 重試（最多 3 次、間隔 5 秒）與未知／用完時開 kill switch（含 `NAKED_LEG_WATCHDOG_LOCK`，Live 解除武裝）並通知
- [x] 2.6 流程測試（模擬執行器）：一腿成交另一腿未成交 → 自動平倉且狀態不變；兩腿皆成交 → 不送單；查詢失敗 → 不送單並鎖定；平倉被拒 3 次 → 鎖定；重啟後還原等待時間

## 3. 頁面

- [x] 3.1 授權 B 文案產生函式（數字取實際設定）與測試
- [x] 3.2 風控設定頁固定的「自動處理規則（授權 B）」區塊
- [x] 3.3 交易單人工處理卡顯示看門狗狀態與下一步；通知文字使用同一份文案

## 4. 驗證

- [x] 4.1 `cargo test` 全套綠燈、`cargo clippy` 無新增警告、`openspec validate naked-leg-watchdog --strict` 通過

## 5. 審查第一輪修正

- [x] 5.1 A1 手動 `FlattenLeg` 與看門狗雙向互斥；`send_flatten` 不覆寫、不重複送同一腿的平倉單（重現測試：第 4 秒手動、第 6 秒放行 → 只有一張平倉單）
- [x] 5.2 A7 讀取類錯誤連續 3 輪才判未知（`READ_FAILED`），實質矛盾立即未知
- [x] 5.3 A8 未知通知與人工處理卡依實際已送出的平倉單數撰寫（列出每張單與成交量）
- [x] 5.4 A9 文案誠實：不寫「通知你」、「最多送出 3 次」、授權 B 區塊補「另一腿已平倉也適用」與提前出場暫停、「兩腿都有部位（可能不平衡）」並列數量
- [x] 5.5 A10 「另一腿為空」明確要求記錄成交 0 且持倉 0（容差 100 的測試）
- [x] 5.6 A11 送單前失敗的嘗試也計入次數，重啟後依事件還原，連續 3 次 → 未知
- [x] 5.7 簡化：刪除 `REPORT_SLACK_MS` 兜底與其測試；`staged_orders.rs` 一律用 `watchdog_text::status_lines`

## 6. 審查第二輪修正

- [x] 6.1 B1 手動 `FlattenLeg` 的 `client_order_id` 與逾時記在 flow（`flattens`）；`user_flatten_active` 只看未逾時的手動單，`send_flatten` 的拒絕只適用未逾時的單；一般平倉的未知殘單不再讓看門狗永遠不跑（回歸測試：走到 PARTIAL_FAILURE 後 300 秒必有看門狗事件並鎖定，先紅後綠；另測殘單逾時後手動可送）
- [x] 6.2 B2 `waited_rounds`／`read_failed_rounds` 各自獨立累計，只在 `finish_round` 一處更新（`RoundCounts::after`，重啟同規則重建）；測試 W,R,W,R… 最終 Unknown（`MAX_ROUNDS_PER_ALERT` 已於 7.2 刪除）
- [x] 6.3 B4 探測任務與手動 `FlattenLeg` 讀取任務加 Drop guard（`LostTaskGuard`）：沒送出結果（含 panic）就改送讀取失敗／清 `flatten_inflight` 並寫 abort；測試以會 panic 的替身驗證
- [x] 6.4 簡化：`watchdog.rs` 模組 doc 與 `WatchdogProbe::unreadable` doc 的過時說明更正；`read_failure` 改私有

## 7. 審查第三輪修正

- [x] 7.2 C2 刪除 `MAX_ROUNDS_PER_ALERT`、`DecideInput.rounds_done` 與 `decide` 包裝：輪數由「W、R 各 3 輪即 Unknown、只有 Flatten 歸零、最多 3 次嘗試」保證有界；測試 W,R 交替最終 Unknown、W,R,W,R 後 3 次平倉都能送出（先紅：有上限時只送得出 2 次）

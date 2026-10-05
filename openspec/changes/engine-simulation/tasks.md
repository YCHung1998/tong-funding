> 前置：確認 `core` 的 `next()` 已涵蓋 design.md Open Questions 第 1 點列出的轉移；若沒有，須先在 `core-domain-and-fixtures` 補上（本 change 不改 core）。
> 每項「改邏輯」的 task 一律先寫測試並確認紅燈（附失敗原因），再實作並確認綠燈；驗收時回報實際指令與測試檔路徑。

## 1. Actor 與通訊

- [ ] 1.1 `Command` / `Event` / `Snapshot` 型別與 `opens_exposure()` 窮舉 match。驗收：先寫「新增變體未分類即編譯失敗」的測試（trybuild 或 clippy `wildcard_enum_match_arm` 於 CI，二選一，先驗證所選方式真的會失敗）與「手動下單依 `reduce_only` 分類」的測試，紅燈後實作；`cargo test -p app engine::command`
- [ ] 1.2 actor 主迴圈：單一擁有者、I/O 以 spawn task 執行並回送 Event、行情走獨立 `watch`、Snapshot 限頻。驗收：先寫三個測試（下單呼叫永遠不回應時 tick 仍被處理、1 秒 1,000 筆行情只推 ≤ 4 份 Snapshot、UI 停止讀取時記憶體不線性成長）並確認紅燈，再實作；使用 tokio 暫停時間，不實際等待
- [ ] 1.3 「先落地再生效」的轉移執行器：呼叫 core `next()`、與 store 同一 transaction 寫狀態與事件、寫入失敗即停機；原子 `add_if_not_pending` 與「已開啟配對」定義。驗收：先寫故障注入測試（寫入失敗時 `Executor` 呼叫 0 次、之後 `opens_exposure` Command 被拒）與「兩 Command 對同標的只成功一個」測試，紅燈後實作

## 2. 排程與流程

- [ ] 2.1 注入的 `Clock` 與 1 秒排程器；結算時間取兩腿較早者並於建立時固定；進場視窗 `[T−15s, T)`、出場 `T+15s`；`serverTime` 偏移校正、偏移不可用不進場、錯過視窗取消。驗收：先寫偏移案例、視窗邊界（`T−15s` 整、`T` 整）與重啟已過結算的測試，紅燈後實作；另有掃描 `engine` 原始碼不含系統時鐘呼叫的測試
- [ ] 2.2 基準價與送單前價格分離抓取；Node 0 呼叫 core 送單前檢查並使用 `effective_for_pair`；設定不完整即 BLOCK。驗收：先寫「兩次抓取各有 `observed_at`」「第二次未更新被擋」「Bybit 覆寫槓桿 4 使 Node 0 失敗」「缺 `est_slippage_pct` 被擋」測試，紅燈後實作
- [ ] 2.3 Node 1–5：數量經 `Quantity`、兩腿送出、成交等待、讀取生效 `order_timeout_seconds`、逾時依 `next()` 分流且零自動補單。驗收：先寫「逾時 7 秒真的在第 7 秒觸發」「一腿 100%、一腿 70% 時 `Executor` 只收到原兩筆」「低於 `min_qty` 的腿不送單」測試，紅燈後實作
- [ ] 2.4 Node 6–8 與 PREPARED 自動撤銷：出場、已平倉確認、停機時仍撤銷、不碰已有曝險的配對。驗收：先寫測試後實作；並以假時鐘跑完整一輪進場至出場，輸出事件序列（貼出）供使用者檢視

## 3. 模式與安全

- [ ] 3.1 `Executor` / `AccountView` 介面、`SimulatedExecutor`（無 client、可腳本化、模擬持倉與委託帳、`sim` 前綴、事件標記模擬）、真實執行器工廠注入與模式切換規則。驗收：先寫「整輪 SIMULATION 工廠呼叫 0 次且網路攔截器 0 請求」「有進行中配對時拒絕切換」「金鑰讀取失敗維持 SIMULATION」測試，紅燈後實作；另附 `cargo tree -p app` 輸出與掃描 `SimulatedExecutor` 模組不引用下單型別的測試
- [ ] 3.2 `trigger_mode` 與 `execution_mode` 獨立開關、單一下單路徑（手動下單與排程共用 `Executor`）、kill switch 只攔 `opens_exposure` 者且不強平、讀取失敗視為停機。驗收：先寫四種模式組合、停機不自行平倉、停機時人工平倉被接受、讀取失敗視為停機的測試，紅燈後實作

## 4. 恢復

- [ ] 4.1 送單前落地意圖與 `client_order_id` 規則（≤ 36 字元、`[A-Za-z0-9_-]`、決定性）；結果未知不換 id 重送、以原 id 查詢。驗收：先寫「意圖先於呼叫」「意圖寫入失敗不呼叫」「逾時後只查詢不重送」測試，紅燈後實作
- [ ] 4.2 重啟對帳（唯讀）與啟動關卡：未結束意圖逐一查詢、依規則表轉 `CANCELLED` / `PARTIAL_FAILURE` / `UNRESOLVED` / 正常、對帳完成前拒絕增加曝險、`sim` 意圖轉 `UNRESOLVED`。驗收：崩潰測試在「意圖已寫入未呼叫」與「已呼叫未寫回」兩時點終止並以同一資料庫檔重啟，斷言未重複下單、狀態符合規則、警示已觸發；回報指令與測試檔路徑

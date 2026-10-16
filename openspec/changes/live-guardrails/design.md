## Context

- kill switch 存在資料庫 `system_flags`（`app/src/store/state.rs` 的 `FLAG_KILL_SWITCH`），`app/src/engine/gate.rs` 的 `refusing`／`admit` 只擋 `opens_exposure()` 的命令，`ManualClose`／`AutoExit` 照常；讀不到時視為 ON；目前引擎不會自己開 kill switch，只有人工或測試會。
- 風控設定在 `core/src/risk.rs`（`RiskConfig`），`min_expected_net_pnl_pct` 與 `early_exit_min_move_pct` 是新增選填欄位的先例；`RiskLimits` 是送單前 10 項檢查之一（`core/src/pretrade.rs`），`max_concurrent_pairs` 已在那裡強制。
- 進場名目來自合約模板 `CONFIG_CONTRACT_TEMPLATE`，儲存時只檢查 > 0（`actor.rs:2553`）；手動下單 `Actor::manual_order` 送出 `notional_usdt: None`（`actor.rs:2026-2071`）；單腿平倉邏輯 `plan_leg_close` 已在 `leg-early-exit` 實作，可重用。
- 引擎目前不知道程式身分；`live-process` 提供 `ProcessIdentity`（`app/src/identity.rs`），由組合根傳入。
- `config set` 命令列工具（`app/src/store/config_cli.rs`）可用 `--db` 直接寫資料庫。
- 設定儲存命令（`SaveRiskSettings`／`UpdateConfig`）不算開倉命令，kill switch 不擋。

## Goals / Non-Goals

**Goals:**
- Live 預設鎖定；解鎖只有一條路（`Arm`），由引擎驗證，不信任畫面。
- 單腿金額、進場筆數有硬上限，且在送單前一刻以實際數量與價格再檢查。
- 出事自動鎖定（只剩平倉）；手動下單不能成為繞過上限的出口。

**Non-Goals:**
- 每日損失上限、自動對帳、手續費與金鑰權限 API、緊急平倉通道、macOS 通知、成交晚於結算的處理、IOC 限價單、`AUTO` 模式（皆延後到開 AUTO 之前的後續變更）。
- 不新增 `ExecutionMode` 變體；不改 Demo 身分的行為。

## Decisions

1. **kill switch 就是武裝狀態**：不另建狀態機。Live 啟動寫入 ON；`Arm` 成功寫入 OFF；自動鎖定寫入 ON。理由：既有語意（只擋開倉）正好符合需求，且 gate 已經對「讀不到」封閉。
2. **`Arm` 的驗證都在引擎**：引擎維護最近一次預檢結果（`preflight_id` 為引擎產生的隨機值＋時間戳＋結果雜湊，保存在記憶體），`Arm` 只接受仍有效且全綠者；確認字串固定為 `我了解這是真錢 LIVE`（正規化後比對）。UI 只負責收集輸入。
3. **預檢以可注入的檢查函式組成**（金鑰存在、唯讀讀取、校時、持倉模式、既有部位、保證金、上限、磁碟），每項回傳 `Pass`／`Fail(原因)`；讀取沿用既有帳戶客戶端與 `ExistingExposure` 的判斷，不新增交易所端點。磁碟空間檢查用資料庫所在檔案系統的可用空間。
4. **上限檢查位置**：(a) `RiskLimits`（進場前檢查）檢查配對名目；(b) 在 `OrderRequest` 建立後、送出前檢查「數量 × 最差保護價」，保護價取該腿基準價與最新價中較高者再加一個固定緩衝（1%）；(c) 儲存合約模板與加入交易單時檢查。reduce-only 一律不受限。替代方案：只檢查模板——被否決（手動單與數量進位都能繞過）。
5. **天花板是程式常數**（每腿 100 USDT、每次武裝 5 筆）：設定檔與資料庫都改不了它，要放寬必須改程式並重新發版。理由：打錯字的最後一道防線。
6. **筆數上限**：在引擎記憶體保存本次武裝的累計進場數（`Arm` 時歸零）；每次配對進入 `ORDER_SUBMIT` 時加一，達到上限就把 kill switch 設為 ON。重啟後 kill switch 預設為 ON，計數自然歸零。
7. **自動鎖定**：集中在一個函式 `lock_live(reason)`（寫 kill switch ON 與 `LIVE_AUTO_LOCK` 事件，同一筆交易）；配對狀態轉移與錯誤處理的幾個既有位置呼叫它。認證錯誤的判定先用「HTTP 401／403 與已知的交易所錯誤碼清單」（清單以實測與官方文件為準，MVP 保守：寧可誤鎖）。
8. **`FlattenLeg`**：重用 `plan_leg_close` 與單腿 `OrderSet`；kill switch 為 ON 時允許；不改變配對狀態（沿用 `leg-early-exit` 的做法），完成後若兩腿皆平，走既有的確認已平倉流程。
9. **設定凍結**：武裝時把上限複製到引擎記憶體 `FrozenCaps`；設定儲存命令在已武裝時比較新舊值，調高即拒絕。命令列工具以身分判斷：開啟的資料庫環境為 live 時，拒絕寫入受保護的鍵（沿用 `live-process` 的資料庫環境中繼資料）。
10. **Live 資料庫初始值**：在 live 資料庫第一次建立時（`live-process` 的中繼資料寫入處之後）寫入初始設定；`trigger_mode = AUTO` 在 live 被拒絕。
11. **前往交易所連結**：純函式產生網址（Binance：`https://www.binance.com/en/futures/<SYMBOL>`；Bybit：`https://www.bybit.com/trade/usdt/<SYMBOL>`；實作時再核對格式），以系統預設瀏覽器開啟；測試只驗證網址內容。
12. **引擎取得身分**：`EngineDeps` 新增 `identity` 欄位，由組合根傳入；Demo 身分所有新行為皆為關閉。

## Risks / Trade-offs

- [預檢依賴交易所唯讀讀取，讀取格式與 demo 不同的風險] → 所有讀取失敗即封閉；階段 0（只讀）專門用來暴露這類差異。
- [自動鎖定條件可能誤鎖（例如暫時性的 429）] → 寧可誤鎖：鎖定只擋開倉，不影響平倉，重新武裝即可。
- [認證錯誤的交易所錯誤碼沒有網路驗證] → 先以 HTTP 狀態與保守的關鍵字判定，階段 0 實測後補齊；列為驗證項。
- [天花板寫死在程式裡，之後放大要發版] → 這是刻意的；階段 3 之後再討論是否改為簽署過的設定。
- [`Arm` 的確認字串固定，使用者可能複製貼上] → 本版不阻止貼上（框架限制）；確認字串較長、需手動輸入並由引擎驗證，視為足夠。
- [設定凍結可能阻礙正常的調低操作] → 調低即時生效，不受影響。

## Migration Plan

1. 先寫失敗的測試（核心設定、引擎指令、gate、上限檢查、自動鎖定、凍結），再實作；Demo 身分的既有測試必須全綠。
2. 部署後 Demo 行為不變；Live 需要你在 live 資料庫填入必填上限才能武裝。
3. 回滾：還原 commit；live 資料庫中的新設定欄位在舊版會被忽略。

## Open Questions

- 確認字串使用 `我了解這是真錢 LIVE` 是否可以？（若你想換成其他固定句子，只是常數。）
- 預檢的磁碟空間門檻 100 MB 是否足夠？

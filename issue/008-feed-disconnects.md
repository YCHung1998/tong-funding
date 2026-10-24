# 008 — 資料源頻繁斷線與 PTB 12:00 被擋：多半是機器與網路，不是交易所

| | |
|---|---|
| 狀態 | 調查完成（唯讀）；**未經三方審查**，結論分「已確認」與「推論」 |
| 日期 | 2026-10-23 |
| 範圍 | `funding.db`（demo）2026-10-14 10:55 ～ 10-22 15:05 UTC 的 FETCH_ERROR 2,491、FEED_RECOVERED 2,278、BLOCKED 1、ENTRY_WINDOW_MISSED 12 |
| 資料來源 | 資料庫唯讀副本、`pmset -g log`（10-16 起）、macOS 系統日誌的網路與 TCP 統計（10-21 起）、程式碼（main b1e6583） |
| 處置 | 未修改程式；修正方向列在最後，待決定 |

## 摘要

- **錯過進場 12 對，全部是 Mac 睡著了**（已確認）：能對到電源紀錄的 7 筆，結算前 10 秒 Mac 都在睡眠；其餘 5 筆前後一小時完全沒有任何事件，特徵相同。
- **斷線大量出現時，網路是 iPhone 個人熱點**（10-21、10-22 已確認，其他日子推論）：
  - Binance WebSocket 錯誤：熱點每清醒小時 52.7 次、13.7% 時間沒資料；家用網路 0.9 次、0.15%。
  - 熱點上 TCP 封包重傳 9.6%（家用 0.9%），往返時間 p90 761 ms（家用 45 ms）。
- **PTB 12:00 被擋**（事實已確認）：送單前檢查在 11:59:49.996 開始、花了 10.5 秒才失敗（17 次成功的中位數 0.58 秒、最長 2.1 秒）。當時 Mac 剛從睡眠醒來並連在熱點上，11:41～12:18 所有來源都在反覆失敗；Bybit 公開 REST 從 11:59:52 起連續逾時。推論逾時的是 Bybit 那腿。
- **持倉期間沒有受影響**：17 筆全部正常平倉；Binance WebSocket 的錯誤不影響下單，因為送單前與出場都另外用 REST 重抓。
- **程式也有放大因素**（程式碼已確認）：
  - Binance WebSocket 3 秒沒有資料就判定過期，對「每秒一個大封包」太嚴；重連退避在反覆斷線時永遠停在 1 秒。
  - 送單前檢查分三段**依序**執行，共用結算前 10 秒的預算；單一標的重抓的 2 秒逾時小於劣質網路的 TCP 重送時間；交易所的商品目錄過期時，重抓還要等目錄（每頁最多 10 秒）。
  - 錯誤只在「開始失敗」與「錯誤種類改變」時寫事件，所以 FETCH_ERROR 2,491 筆不等於斷線 2,491 次；但 FETCH_ERROR + FEED_RECOVERED 已佔事件表約 78%。

## 已確認 vs 推論

| 說法 | 狀態 |
|---|---|
| 12 筆錯過進場是 Mac 睡眠 | 7/7 可對照的筆數已確認；5 筆依特徵推論 |
| 熱點期間錯誤率與不可用時間大幅高於家用網路 | 10-21、10-22 已確認；10-15、10-16、10-20 的日間爆量推論為同型態（系統日誌已輪替） |
| Binance WebSocket 錯誤與 TCP 亂序位元組高度相關（r = 0.915，熱點；家用 0.005） | 已確認（統計） |
| 為何只有 Binance WebSocket 大量過期：大封包尾端遺失要等 TCP 重送逾時 | 推論 |
| 10-19 有 15 分鐘「Binance WebSocket 連得上卻沒有有效資料」 | 事實已確認；原因不明（程式分不出「沒收到」與「收到但解析失敗」） |
| PTB 那腿是 Bybit、10.5 秒花在哪一段 | 推論（沒有分段計時） |
| `acct_mode_unsupported`（OKX 帳戶模式）佔 okx_account 錯誤 64 筆 | 已確認；是設定問題不是斷線 |

## 修正方向（未實作，需要決定）

1. **機器**：真錢之前不要在會睡眠的筆電上跑；改用常駐主機。至少在有排隊配對或結算前幾分鐘阻止系統睡眠，並在錯過進場的事件裡記錄「是否剛睡醒」。
2. **網路閘門**：武裝或進場前檢查近期的不可用比例與往返時間，偵測到熱點或計費網路時警告或擋下；事件記錄出口 IP（API 金鑰綁 IP 時，換網路會讓簽名請求失敗，10-16 已發生一次）。
3. **送單前檢查**：三段改成並行，或把保證金、下單規則、外部部位、槓桿上限提前到結算前 60 秒預取，結算前 10 秒只更新價格；整體設硬預算並允許換新連線重試一次；重抓不要被目錄更新卡住。
4. **WebSocket**：把「沒收到資料」「收到但無效」「延遲」分開記錄；過期門檻分兩段（先警告、再重連），連線穩定一段時間才重置退避並加隨機抖動；斷線事件改成按分鐘彙總，避免灌爆事件表。
5. **可觀測性**：記錄 HTTP 錯誤的細節與各階段耗時；設定連線逾時、TCP keepalive、較短的連線池閒置時間。

## 重現

分析腳本與中間產物在當次工作階段的 scratchpad（`scripts/`、`out/`），資料庫副本截至 10-22 17:34 UTC。關鍵查詢：

```sql
select json_extract(payload,'$.source') src, count(*) from events where event_type='FETCH_ERROR' group by 1;
select pair_id, ts_ms, payload from events where event_type='PAIR_TRANSITION' and json_extract(payload,'$.to')='BLOCKED';
select ts_ms, payload from events where event_type='ENTRY_WINDOW_MISSED';
```

網路時間軸：`/usr/bin/log show --predicate 'process == "configd" AND eventMessage CONTAINS "network changed: v4("'`；電源：`pmset -g log`。

## 相關程式位置

- `app/src/exchange/health/feed.rs`：WebSocket 連線、過期判定（`check_silence`、`max(stale_data_threshold_ms, 3×週期)`）、退避
- `app/src/engine/actor.rs`：`start_entry`（送單前三段依序執行）、`entry_still_allowed`
- `app/src/exchange/public/adapter.rs`、`bybit.rs`、`binance.rs`：單一標的重抓與目錄的逾時
- `app/src/exchange/reqwest_transport.rs`：HTTP client 設定

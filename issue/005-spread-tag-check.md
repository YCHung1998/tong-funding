# 005 價差標籤對 8 筆實單的核對（spread-position-tag 任務 4.2）

只呈現資料，不推論因果。資料庫唯讀（`~/Library/Application Support/tong-funding/funding.db`），重現指令：

```
python3 issue/tools/spread_tag_check.py          # 全部已平倉配對
python3 issue/tools/spread_tag_check.py rsr      # 只看符合關鍵字的
```

## 定義

- 標籤與 `core/src/spread_tag.rs` 相同：多腿所在交易所價格 < 空腿所在交易所價格 = `positive`；> = `negative`；相等 = `neutral`。百分比 = (空 − 多) ÷ 多 × 100，不四捨五入（此表顯示 4 位）。
- 三個觀測點：
  - **T-10 快照**：`PAIR_TRANSITION`（to=`ORDER_SUBMIT`）的 `entry_snapshot`，兩腿的 `mark_price`（缺時用 `expected_price`）。這是 spec 要求「進場那一筆」會用的價格。
  - **進場成交價**：`ORDER_FILL`（action=open）的 `avg_price`，多腿成交價 vs 空腿成交價。
  - **出場成交價**：`ORDER_FILL`（action=close）的 `avg_price`。
- 「價差百分比 出場−進場」= 出場成交價標籤的百分比 − 進場成交價標籤的百分比。
- 價差損益（real）= 多腿 (平倉價 − 開倉價) × 數量 + 空腿 (開倉價 − 平倉價) × 數量，與 `issue/tools/trade_table.py` 的「價差損益（real）」同一定義。單位 USDT。
- 注意：T-10 快照用的是標記價；成交價欄位用的是實際成交均價，兩者來源不同，不可直接互相比較數值。

## 結果（8 筆，皆已 CLOSE_CONFIRMED）

| 配對 | 多腿 / 空腿 | T-10 快照標籤 | 進場成交價標籤 | 出場成交價標籤 | 價差百分比 出場−進場 | 價差損益（real，USDT） |
|---|---|---|---|---|---|---|
| `ui-1791992834087-0-arcusdt` | Bybit / Binance | positive +0.2323% | positive +0.1627% | positive +0.5541% | +0.3914% | −3.8921 |
| `ui-1791982970031-0-blurusdt` | Bybit / Binance | negative −0.1433% | negative −0.0197% | positive +0.2273% | +0.2471% | −2.4630 |
| `ui-1791993495450-0-1000catusdt` | Bybit / Binance | negative −0.2939% | negative −0.2674% | negative −0.2139% | +0.0535% | −0.5343 |
| `ui-1792027746054-0-rsrusdt` | Bybit / Binance | negative −0.3288% | negative −0.5228% | negative −0.3094% | +0.2134% | −2.1418 |
| `ui-1792036763773-1-rsrusdt` | Bybit / Binance | negative −0.8617% | negative −0.8987% | negative −0.8499% | +0.0489% | −0.4806 |
| `ui-1792036763773-3-walusdt` | Bybit / Binance | negative −0.3544% | negative −0.3798% | positive +0.0693% | +0.4491% | −4.4905 |
| `ui-1792036763773-0-blurusdt` | Binance / Bybit | negative −0.1697% | positive +0.0800% | positive +0.1904% | +0.1104% | −1.0978 |
| `ui-1792036763773-2-sirenusdt` | Binance / Bybit | positive +0.5502% | positive +0.4581% | positive +0.5044% | +0.0463% | −0.4560 |

## 直接可數的事實

- T-10 快照：positive 2 筆、negative 6 筆，無 neutral。
- 進場成交價：positive 3 筆、negative 5 筆。
- 出場成交價：positive 5 筆、negative 3 筆。
- T-10 快照標籤與進場成交價標籤相同者 7 筆；不同者 1 筆（`ui-1792036763773-0-blurusdt`，Binance 多／Bybit 空：negative → positive）。
- 8 筆價差損益全部為負值（−0.4560 至 −4.4905 USDT）；「價差百分比 出場−進場」全部為正值（+0.0463% 至 +0.4491%）。

## 未解決／限制

- 這是 8 筆樣本，且每一筆都在標記價與成交價有各自差距，無法在此判斷 `positive`／`negative` 對價差損益的方向性是否成立；tasks 4.2 的要求只是並列呈現。
- 本表沒有「持倉中途」的標籤（只有進場前與兩個成交時點），`SPREAD_TAG` 事件（task 3.4，由主流程實作）上線後才會有進場成交後與平倉前的記錄可核對。

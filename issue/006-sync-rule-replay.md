# 006 套用「兩腿結算時間必須相同」規則重放 2026-10-14／15 八筆實單

- 變更：`openspec/changes/require-synchronized-settlement`（task 4.3）
- 指令（唯讀）：`python3 issue/tools/check_sync_rule.py`
- 資料：`~/Library/Application Support/tong-funding/funding.db`，每筆配對取送單前檢查通過時的進場快照（`PAIR_TRANSITION` → `ORDER_SUBMIT` 的 `entry_snapshot`），比較兩腿 `next_funding_time`。

| 配對 | long 下次結算 (UTC) | short 下次結算 (UTC) | 新規則 |
|---|---|---|---|
| ui-1791982970031-0-blurusdt | 10-14 16:00 | 10-14 16:00 | 可進場 |
| ui-1791992834087-0-arcusdt | 10-14 16:00 | 10-14 16:00 | 可進場 |
| ui-1791993495450-0-1000catusdt | 10-14 16:00 | 10-14 16:00 | 可進場 |
| ui-1792027746054-0-rsrusdt (02:00) | 10-15 02:00 | 10-15 08:00 | 擋下 |
| ui-1792036763773-0-blurusdt (04:00) | 10-15 04:00 | 10-15 08:00 | 擋下 |
| ui-1792036763773-1-rsrusdt (04:00) | 10-15 04:00 | 10-15 08:00 | 擋下 |
| ui-1792036763773-2-sirenusdt | 10-15 04:00 | 10-15 04:00 | 可進場 |
| ui-1792036763773-3-walusdt | 10-15 04:00 | 10-15 04:00 | 可進場 |

結論：8 筆中 3 筆（RSR 02:00、RSR 04:00、BLUR 04:00）兩腿結算時間不同而被擋，其餘 5 筆維持可進場，與 design.md 的預期一致。

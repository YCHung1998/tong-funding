# Issues

專案實際遇到的問題記錄：現象、證據、原因、決策與教訓。已確認與推論分開標示。

| # | 標題 | 狀態 | 處置 |
|---|---|---|---|
| [001](001-binance-demo-funding-rate.md) | Binance demo 模擬倉的資金費率與正式環境不一致 | 原因已確認；不實作（demo 環境限制） | 不修改程式 |
| [002](002-trades-2026-10-14.md) | 2026-10-14/15 四筆實單整理（Bybit 流水延遲、手續費設定偏低） | 已整理；Bybit 流水延遲原因未知 | 未修改程式 |
| [003](003-bybit-funding-ledger-delay.md) | Bybit demo 的資金費流水延遲超過 1 小時才出現 | 已記錄；原因未知，Eason 研究中 | 未修改程式 |
| [004](004-trades-2026-10-15-0400.md) | 2026-10-15 04:00 四筆實單整理（Binance demo 04:00 無結算、SIREN 的 Bybit 手續費為兩倍） | 已整理；4 筆皆 COMPLETE | 未修改程式 |
| [005](005-spread-tag-check.md) | 價差標籤（spread-position-tag）對 8 筆實單的並列核對 | 已呈現；不推論因果 | 未修改程式 |
| [006](006-sync-rule-replay.md) | 「兩腿結算時間必須相同」規則重放 8 筆實單（3 筆被擋） | 已呈現 | 未修改程式 |

制式分析表與產生工具：[template-trade-table.md](template-trade-table.md)、`tools/trade_table.py`。

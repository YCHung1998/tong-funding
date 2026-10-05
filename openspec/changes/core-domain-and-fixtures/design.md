## Context

來源：`mvp-python`（HANDOFF.md 為準）與 2026-10-05 的三方抗辯（skeptic / red-team / simplifier）。
`core` 是純邏輯 crate：不做 I/O、不讀系統時鐘、不依賴 GPUI。時間與設定一律由呼叫端注入。

## Goals / Non-Goals

**Goals**
- 把 Python 版的純邏輯搬成 Rust，並修掉抗辯確認的設計缺陷。
- 讓之後的 store / exchange / engine 只需「呼叫 core」，不必重新決定業務規則。

**Non-Goals**
- 不做 HTTP、WebSocket、SQLite、Keychain、排程。
- 不做 Funding PnL（`funding-pnl` change）。
- 不匯出整合行為的 fixtures。

## Decisions

**D1　Decimal 全面取代浮點。**
Python 版用 `1e-9` epsilon 與 `round(..., 12)` 補浮點誤差（`quantity_precision.py`）。
Decimal 不需要這些補丁，也避免 `scanner.py` 的 banker's rounding 差異被帶進 Rust。
代價：與 Python 輸出可能在極端案例不同，由差異對照表登記。

**D2　Net Edge 採單次結算，不用 8h 等效。**
Python 版的持倉模型是「結算前 15 秒進、結算後 15 秒出」，只經歷一次結算（`auto_stage.py`）。
8h 等效會把兩腿都算成賺到，但週期不同時只有一腿會結算。
8h 等效保留為純顯示函式，不進判斷。

**D3　`max_slippage_pct` 拆成兩個欄位。**
`max_price_drift_pct` 是上限（`pretrade_check.py` 用來擋漂移），保守＝取小；
`est_slippage_pct` 是成本（進 Net Edge），保守＝取大。方向相反，不能共用欄位與合併規則。

**D4　Pair 狀態轉移是單一純函式。**
「`PARTIAL_FAILURE` 不得自動轉出」是 HANDOFF 的不變規則。放在單一函式裡，才能用測試窮舉驗證，
不必靠 code review 記得。使用者在 2026-10-05 拍板為「完全人工」，所以 SYSTEM_SPEC §27 的自動補平衡不採用。

**D5　`Quantity` 只能經取整函式建構，平倉例外。**
平倉要用交易所回報的實際持倉（`trade_pipeline.py:138-160`），重新取整可能殘留零頭，所以另有「由交易所持倉建構」的路徑。

**D6　費率沒有內建預設。**
我沒有查各交易所目前的 taker 費率，內建任何數字都等於憑空捏造。缺失時回傳錯誤，讓 UI 顯示「未設定」。

## 與 Python 版的差異對照表

| 項目 | Python 版 | Rust 版 | 原因 |
|---|---|---|---|
| 數值型別 | float + epsilon | Decimal | D1 |
| 達標 | gross spread 為主 | Net Edge | D2、使用者拍板 |
| `max_slippage_pct` | 單一欄位 | `max_price_drift_pct` ＋ `est_slippage_pct` | D3 |
| `hedge_threshold_pct` | 99 | 移除 | 與 `max_leg_imbalance_pct` 重複 |
| `funding_threshold_pct` | 0.20 | 移除 | 達標改由 Net Edge 決定 |
| `max_concurrent_trades` | 5（legs） | 移除 | 與 `max_concurrent_pairs` 重複 |
| `execution_mode` | `SIMULATION` / `LIVE` | `SIMULATION` / `EXCHANGE_DEMO` | 不做真錢，避免誤解 |
| `stale_data_threshold_ms` | 1000 | 1000 | 使用者拍板維持；Figma 的 5 秒要改 |
| `safety_margin_pct` | 無 | 預設 0.01（百分比數值） | 使用者拍板；Net Edge 新增欄位 |
| 資料過期檢查 | 無（`app.py:_get_latest_price` 不看連線狀態） | `DataFresh` | red-team |
| 持倉衝突檢查 | 無（`confirm_fills` 只看有無持倉） | `ExistingExposure` | red-team |
| 風控覆寫 | 只存不讀（Fragility #1） | 純函式合併，供執行路徑呼叫 | 使用者保留此功能 |

## Risks / Trade-offs

- **Net Edge 可能讓幾乎沒有標的達標。** 依一般 taker 費率，4 筆成交的成本可能高於單次結算的 funding。這是模型誠實的結果，不是 bug；**費率為未驗證的推測**，需使用者填入實際值後才能判斷。
- **`stale_data_threshold_ms` 預設 1000 ms 對 REST 輪詢偏緊**（Bybit、OKX 每 10 秒才抓一次，Python 版 README），
  若 Net Edge 或送單前檢查直接用輪詢資料，幾乎必然過期。已拍板維持 1000，但 `exchange-readonly-adapters` 必須讓送單前檢查**重新抓取**資料，而不是沿用輪詢快取。
- **`est_slippage_pct` 按四筆成交各估一次**是我的簡化假設，真實滑價與下單量、深度有關，待 `funding-pnl` 後以實際成交資料校正。

## Open Questions

（無。`stale_data_threshold_ms` = 1000 與 `safety_margin_pct` = 0.01 已於 2026-10-05 由使用者拍板。）

## 決定紀錄（2026-10-05，使用者）

- **轉移表補完**：`engine-simulation` 與 `exchange-demo-execution` 的 spec 需要的轉移（`PREPARED → CANCELLED`、`FILL_MONITOR → IMBALANCED`、`CLOSING → PARTIAL_FAILURE`、重啟對帳進入 `PARTIAL_FAILURE` / `UNRESOLVED`、人工確認已平倉的目標狀態）原本沒有列在 `pair-lifecycle`；由本 change 補上完整轉移表（見 spec），其他 change 不改 core。
- **Decimal 型別**：`Price`、`Rate`、`Notional` 以 `rust_decimal::Decimal` 的型別別名實作，不另建包裝型別（YAGNI）；`Quantity` 仍是 newtype。

## Why

`okx-demo-execution` 讓 OKX 能在 demo 送單，但對抗審查（red-team）指出幾個「真的送單」前必須補上的守衛：OKX demo 與正式共用主機，標頭只證明我們的請求自稱 demo，並不證明手上的金鑰是 demo 金鑰；OKX 的 `sz` 是張數、不同標的 `ctVal` 差距極大（BTC 0.01、部分幣 1000+），幣量誤當張數會直接放大；`50004` 逾時後立刻查到 `51603` 不代表單不會成交；未知的 `sCode` 不應被當成「明確拒絕」；平倉被拒會留下裸露的一腿。這些守衛不改變 `okx-demo-execution` 的行為契約，只在送出前 / 判讀時加上 fail-closed 的條件，因此獨立成一個 change。

## What Changes

- **demo 金鑰正向證明與 `50101` 閂鎖**：第一筆 OKX 單之前，必須以同一組金鑰實例對帳戶設定送出帶標頭的 GET 並得到 `code "0"`；證明前 OKX 訂單 `not_sent`。任一處收到 `50101`（環境不符）即閂鎖：本程序內停用 OKX（送單 `not_sent`、查單失敗、讀取端回錯），並以明確原因文字呈現（不是一般的 Rejected）。
- **數量守衛**：OKX 送單前由 `OkxLimitsSource` 取得該標的的 `ctVal`、`lotSz`、標記價與單腿名目上限；缺任一項、`sz` 不是 `lotSz` 整數倍、或 `sz × ctVal × 價格` 超過上限 → `not_sent`。
- **`expTime` 與雙重 `51603`**：OKX 送單帶 `expTime`；對「結果未知 / 被限流」的單，`51603` 只有在伺服器時間超過 `expTime` 且連續兩次 `51603` 後才算查無，否則維持待確認。
- **拒絕碼白名單**：只有文件列出的拒絕碼算「已拒絕」；未列出的 `sCode`、或 `code "0"` 但 `data` 為空 / 格式錯誤，一律是結果未知（以查單確認）。
- **平倉守衛**：`reduceOnly` 的 OKX 單每次重讀帳戶模式（不用快取）；平倉被 `51000` / `51010` 拒絕時，原因文字明確指出對側腿裸露、需人工處理。
- **不經 `GatedTransport`**：OKX 下單路徑不得包在 `GatedTransport`（它會替請求排隊 / 重試）；以靜態檢查與測試鎖定；`50013` 送單 = 結果未知且只送一次。
- **實機探針**：`live_probe.rs` 加入 OKX 腿（`TONG_DEMO_EXCHANGES`、校時、以公開 `ctVal` 把幣量換成張數並同時印出兩種單位）。

## Capabilities

### New Capabilities
- `okx-execution-guards`: OKX 下單前後的 fail-closed 守衛：金鑰正向證明與環境閂鎖、數量與名目守衛、逾時單的查無判定、拒絕碼白名單、平倉守衛。

### Modified Capabilities
（無已封存的相關 spec。本 change 收緊 `okx-order-execution`「OKX 送單結果分類」中「其他明確的非零 `sCode` → 已拒絕」為白名單，並補上送單前守衛；該 change 封存時一併合併。）

## Impact

- 修改 `execution/{okx,classify,executor,factory,live_probe}.rs`、`signed/okx.rs`（`OkxLatch`）、`static_checks.rs`；新增 `OkxLimitsSource`（`execution/okx.rs`）。
- fixtures：沿用 `app/tests/fixtures/okx/orders/`（`place_code0_empty_data`、`place_scode_unlisted`、`place_50101`）。
- `okx-trading-enablement` 需接線：`OkxLimitsSource`（公開 instruments + 風險設定的單腿名目上限）、`OkxLatch` 共用與畫面橫幅。

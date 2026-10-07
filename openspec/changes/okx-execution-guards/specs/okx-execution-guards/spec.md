## ADDED Requirements

### Requirement: 第一筆 OKX 單之前必須以同一組金鑰證明是 demo 金鑰

OKX 客戶端 SHALL 在送出任何 OKX 訂單之前，以與下單相同的金鑰實例送出帶 `x-simulated-trading: 1` 的 `GET /api/v5/account/config` 並收到 `code "0"`；在證明之前送單 SHALL 不送出（`not_sent`）。

#### Scenario: 尚未證明

- **WHEN** 新建立的 OKX 客戶端直接被要求送單
- **THEN** 不送出任何請求，結果為已拒絕（`not_sent`），原因指出 demo 金鑰尚未證明

#### Scenario: 閘門讀取即證明

- **WHEN** 單向模式閘門以同一客戶端讀取帳戶設定並得到 `code "0"`
- **THEN** 之後的送單被送出，且送單請求與設定請求帶相同的 `OK-ACCESS-KEY`

### Requirement: 任一處收到 50101 即停用 OKX

任何 OKX 請求（送單、查單、撤單、帳戶設定、唯讀查詢）收到 `50101` 時，系統 SHALL 閂鎖：本程序其後的 OKX 送單 SHALL 不送出、查單與撤單 SHALL 回報失敗、唯讀查詢 SHALL 回報錯誤，原因文字 SHALL 指出環境不符；閂鎖 SHALL NOT 被當成一般的已拒絕訂單，SHALL NOT 在程序內被解除。

#### Scenario: 查單收到 50101

- **WHEN** 一次 OKX 查單回應 `50101`
- **THEN** 其後的 OKX 送單不送出請求，原因含 `50101`

### Requirement: OKX 數量守衛

OKX 送單前 SHALL 取得該標的的 `ctVal`、`lotSz`、標記價與單腿名目上限；任一缺少、`sz` 不是 `lotSz` 的整數倍、或 `sz × ctVal × 標記價` 超過上限時 SHALL 不送出（`not_sent`）並指出原因。沒有任何限制來源時 SHALL 一律不送。

#### Scenario: 幣量被當成張數

- **WHEN** 標的 `ctVal` 為 1000、`lotSz` 為 1，以 `sz = 0.5`（幣量）送單
- **THEN** 不送出，原因指出 `sz` 不是 `lotSz` 的整數倍

#### Scenario: 名目超過上限

- **WHEN** `sz × ctVal × 標記價` 大於單腿名目上限
- **THEN** 不送出，原因指出名目超限

### Requirement: 逾時單的查無判定

OKX 送單 SHALL 帶 `expTime`。對結果為未知或被限流的送單，其 `51603` 查詢 SHALL 在伺服器時間超過 `expTime` 且已連續兩次 `51603` 之後才視為查無；在此之前 SHALL 回報待確認（失敗）。

#### Scenario: 逾時後立刻查無

- **WHEN** 送單回應 `50004`，立即查單得到 `51603`
- **THEN** 查單回報待確認，不是查無

#### Scenario: 過期且兩次查無

- **WHEN** 伺服器時間已超過 `expTime`，連續兩次查單皆為 `51603`
- **THEN** 第二次回報查無

### Requirement: 只有白名單中的代碼才是已拒絕

OKX 送單回應的 `code` / `sCode` 只有在文件列出的拒絕碼名單內才 SHALL 歸為已拒絕；其他代碼、`code "0"` 但 `data` 為空或格式錯誤，SHALL 歸為結果未知並以 `clOrdId` 查單確認。

#### Scenario: 未列出的 sCode

- **WHEN** 回應 `sCode` 為不在名單內的代碼
- **THEN** 結果為未知，不是已拒絕

#### Scenario: code 0 但沒有資料

- **WHEN** 回應 `code "0"` 且 `data` 為空陣列
- **THEN** 結果為未知

### Requirement: 平倉守衛

`reduceOnly` 的 OKX 單 SHALL 在送出前重新讀取帳戶模式（不使用快取）；被 `51000` 或 `51010` 拒絕時，拒絕原因 SHALL 指出對側腿裸露、需人工處理。

#### Scenario: 平倉不用快取

- **WHEN** 單向模式讀數仍在 60 秒內，要求送出一筆 `reduceOnly` 的 OKX 單
- **THEN** 送出前仍重新請求帳戶設定

### Requirement: OKX 下單路徑不經 GatedTransport

OKX 下單 SHALL 直接使用下單傳輸層，SHALL NOT 包在會排隊或重試的 `GatedTransport` 內；`50013` 的送單結果 SHALL 為未知且只送出一次請求。

#### Scenario: 50013

- **WHEN** 送單回應 `50013`
- **THEN** 結果為未知，且只有一次送單請求

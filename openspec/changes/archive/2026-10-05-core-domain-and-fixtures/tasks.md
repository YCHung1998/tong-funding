## 1. 基礎型別

- [x] 1.1 加入 `rust_decimal`、`serde`、`serde_json`；定義 Decimal 包裝型別（Price、Rate、Notional）與 `FundingObservation`、`DataStatus`、雙時間戳。先寫測試（兩時間戳獨立保存）並確認紅燈，再實作；`cargo test -p core` 並以 `cargo tree -p core` 證明仍無 `gpui`
- [x] 1.2 週期推導（Binance 小時、Bybit 分鐘、OKX 時間差）與 `DATA_ERROR` 規則；測試涵蓋三所案例與「查不到不猜 8h」
- [x] 1.3 過期判定（注入時鐘；等於門檻不過期、多 1 毫秒即過期）、8h 等效顯示函式、配對結算時間與「哪些腿會結算」

## 2. Net Edge

- [x] 2.1 Net Edge 計算、達標判定、多空方向、三所取最高、缺費率回傳錯誤。測試先寫三個手算案例（同時結算、週期不同只算一腿、rate 同號相互抵銷）與成交量缺失視為 0，確認紅燈再實作

## 3. 搬移的純邏輯

- [x] 3.1 `Quantity`：向下取整、字串位數、OKX 張數換算、由交易所持倉建構；測試涵蓋 spec 中全部數值案例
- [x] 3.2 送單前檢查：10 項具名檢查，每項各一個「單獨失敗」測試，加「多項同時失敗全部列出」
- [x] 3.3 持倉分組

## 4. 狀態機與風控

- [x] 4.1 Pair 狀態 enum 與 `next()`：寫「窮舉所有非人工事件於三個鎖定狀態皆回錯」的測試，確認紅燈再實作
- [x] 4.2 風控設定型別、預設值、驗證、「不完整」判定
- [x] 4.3 每腿保守值合併純函式；測試涵蓋取小、取大、無覆寫、不可覆寫欄位被拒絕

## 5. Fixtures

- [x] 5.1 `tools/dump_fixtures.py`：唯讀匯出 `quantity_precision`、`pretrade_check`（漂移／保證金／槓桿）、`position_grouping`，檔頭記錄 commit 與來源函式；跑兩次證明決定性，並以 `git -C mvp-python status` 證明未修改來源
- [x] 5.2 Rust 端載入 fixtures 逐筆比對的測試；把有意差異登記到 `design.md` 對照表與測試例外清單；回報實際指令 `cargo test -p core` 與案例總數

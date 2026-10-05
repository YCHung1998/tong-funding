## ADDED Requirements

### Requirement: Fixtures 由 Python 純函式匯出，且只涵蓋純函式

`tools/dump_fixtures.py` SHALL 讀取 `mvp-python` 的純函式（`quantity_precision`、`pretrade_check` 的價格漂移／保證金／槓桿三項、`position_grouping`），
以固定輸入呼叫後把輸入與輸出寫成 JSON，放在 `core/tests/fixtures/`。
該腳本 SHALL 對 `mvp-python` 唯讀，SHALL NOT 修改其任何檔案。
Net Edge、funding 週期推導、Pair 狀態機在 Python 版沒有對應實作，SHALL NOT 產生 fixtures，改以 Rust 端手算期望值的測試覆蓋。
整合行為（排程器、送單流程、帶狀態的假 client）SHALL NOT 納入 fixtures。

#### Scenario: 匯出內容限於純函式

- **WHEN** 執行匯出腳本
- **THEN** 產生的 fixtures 只對應上述三個模組，不含排程器或 client 的整合案例

#### Scenario: 對來源專案唯讀

- **WHEN** 匯出腳本執行完畢
- **THEN** `mvp-python` 的 git 狀態與執行前相同（無新增、修改或刪除）

### Requirement: Fixtures 記錄來源並以字串保存數值

每個 fixtures 檔 SHALL 在檔頭記錄來源：`mvp-python` 的 git commit、來源檔名與函式名、匯出時間。
所有數值 SHALL 以字串保存，Rust 端 SHALL 以 Decimal 解析並比對，不得經過浮點數。
匯出 SHALL 是決定性的：相同輸入重跑兩次，除匯出時間外位元組完全相同。
Python 的顯示用四捨五入（例如 banker's rounding）SHALL NOT 納入比對範圍。

#### Scenario: 來源可追溯

- **WHEN** 開啟任一 fixtures 檔
- **THEN** 檔頭有 commit hash、來源函式與匯出時間

#### Scenario: 重跑結果一致

- **WHEN** 連續執行匯出腳本兩次
- **THEN** 除匯出時間欄位外，兩次產出的檔案內容相同

### Requirement: Rust 端逐筆比對並在不一致時失敗

`cargo test -p core` SHALL 載入每一個 fixtures 檔，對每個案例呼叫對應的 Rust 實作並比對輸出。
任一案例不一致 SHALL 使測試失敗，並在訊息中指出案例編號與差異。
Fixtures SHALL NOT 以人工修改來讓測試通過；有意的行為差異 SHALL 記錄在 `design.md` 的差異對照表，並在測試中以明確的例外清單處理。

#### Scenario: 全部一致

- **WHEN** 執行 `cargo test -p core`
- **THEN** 所有 fixtures 案例通過，輸出列出載入的案例總數

#### Scenario: 有意差異必須登記

- **WHEN** Rust 實作與 Python 輸出在某案例上刻意不同（例如 Rust 版用 Decimal 而沒有 epsilon）
- **THEN** 該案例必須出現在 `design.md` 的差異對照表與測試例外清單中，否則測試失敗

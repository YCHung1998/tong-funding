## ADDED Requirements

### Requirement: 程式身分由啟動參數決定且程序期間不變

程式 SHALL 有且只有兩種身分：`Demo`（預設）與 `Live`。身分 SHALL 只由啟動參數 `--live` 決定；SHALL NOT 由環境變數、設定檔、資料庫內容或執行期的 UI 操作決定或改變。`--live` 搭配 `--bench-*` 等非交易子命令 SHALL 被拒絕並回報原因，程式不啟動交易引擎。

#### Scenario: 預設是 Demo

- **WHEN** 不帶 `--live` 啟動
- **THEN** 身分為 `Demo`，行為與本變更之前相同

#### Scenario: 環境變數不影響身分

- **WHEN** 設定任何名稱含 `LIVE` 的環境變數但不帶 `--live`
- **THEN** 身分仍為 `Demo`

#### Scenario: 身分在執行期間不可改變

- **WHEN** 程式已啟動
- **THEN** 沒有任何指令、設定或 UI 動作可以改變身分

### Requirement: 同一時間只能有一個程式實例

程式 SHALL 在啟動時取得一個與資料庫無關的單一實例鎖；取不到鎖時 SHALL 不啟動引擎、不開資料庫，並顯示明確訊息。鎖 SHALL 不分身分：已有一個 Demo 程式在執行時，Live 程式也 SHALL 無法啟動，反之亦然。鎖在程序結束時（含當機）SHALL 自動釋放。

#### Scenario: 第二個實例被拒絕

- **WHEN** 一個程式已在執行，再啟動另一個（任一身分）
- **THEN** 第二個啟動失敗並說明已有實例在執行

#### Scenario: 當機後可重新啟動

- **WHEN** 持有鎖的程序被強制結束
- **THEN** 下一個程式可以取得鎖

### Requirement: 簽名請求的主機政策依身分分離

傳輸層 SHALL 為 Demo 與 Live 各有一條簽名請求主機政策：Demo 政策 SHALL 只放行既有的 demo／testnet 主機並拒絕任何 live 主機；Live 政策 SHALL 只放行 live 主機（Binance `fapi.binance.com`、Bybit `api.bybit.com`）並拒絕 demo 主機。Live 主機清單 SHALL 與現有的簽名白名單分開定義，SHALL NOT 被併入。Demo 身分 SHALL NOT 建立 Live 傳輸，Live 身分 SHALL NOT 建立 Demo 傳輸。

#### Scenario: Demo 傳輸拒絕 live 主機

- **WHEN** Demo 政策檢查 `https://fapi.binance.com/...` 或 `https://api.bybit.com/...`
- **THEN** 回傳不允許

#### Scenario: Live 傳輸拒絕 demo 主機

- **WHEN** Live 政策檢查 `https://demo-fapi.binance.com/...`、`https://testnet.binancefuture.com/...` 或 `https://api-demo.bybit.com/...`
- **THEN** 回傳不允許

#### Scenario: 公開傳輸不得夾帶簽名請求

- **WHEN** 公開政策檢查一個帶有 `X-MBX-APIKEY`、`X-BAPI-API-KEY`、`OK-ACCESS-KEY` 標頭，或查詢字串含 `signature=`／`sign=` 的請求（即使主機在公開白名單內）
- **THEN** 回傳不允許

#### Scenario: 簽名請求誤走公開傳輸在編譯期失敗

- **WHEN** 程式碼把公開傳輸型別傳給需要簽名傳輸型別的客戶端建構子
- **THEN** 無法通過編譯

### Requirement: Live 主機字串只有一個來源

正式主機字串 SHALL 只出現在 `signed/endpoints.rs`（live 簽名主機）與既有的公開客戶端（公開行情主機）。靜態檢查 SHALL 掃描整個 `src` 的非測試程式碼，任何其他檔案出現正式主機字串 SHALL 使測試失敗。

#### Scenario: 其他檔案寫入正式主機字串被抓到

- **WHEN** 在 `engine/` 或 `store/` 的非測試程式碼加入字串 `fapi.binance.com`
- **THEN** 靜態檢查測試失敗

### Requirement: 限頻分類依呼叫端型別

限頻器 SHALL 依「發出請求的客戶端型別」（簽名／公開批次／公開單一標的）分類，SHALL NOT 依主機是否在簽名白名單判斷。Live 身分下，同一台主機上的公開行情請求與簽名請求 SHALL 各自被正確分類。

#### Scenario: 同主機的公開與簽名請求分開分類

- **WHEN** Live 身分向 `fapi.binance.com` 送出一個公開行情請求與一個簽名的持倉查詢
- **THEN** 前者依公開規則分類（批次或單一），後者分類為簽名

### Requirement: 資料庫與程式身分綁定

Live 身分 SHALL 使用資料庫檔案 `funding-live.db`，Demo 身分 SHALL 使用 `funding.db`。每個資料庫 SHALL 儲存不可變的環境中繼資料（`demo` 或 `live`）：新建立的資料庫由當下身分寫入；Demo 身分首次開啟既有、沒有中繼資料的 `funding.db` SHALL 標為 `demo`；Live 身分開啟沒有中繼資料的既有資料庫 SHALL 被拒絕。開檔時中繼資料與程式身分不符 SHALL 使系統停機（不讀不寫）並回報原因。Schema 版本 SHALL 提升，使舊版程式以「版本過新」拒絕開啟。命令列工具的 `--db` 路徑 SHALL 受同一規則約束。

#### Scenario: Live 身分預設使用獨立檔案

- **WHEN** 以 `--live` 啟動且沒有指定資料庫路徑
- **THEN** 開啟（或新建）`funding-live.db`，中繼資料為 `live`

#### Scenario: 環境不符即停機

- **WHEN** Live 身分被指向一個中繼資料為 `demo` 的資料庫
- **THEN** 系統停機並說明環境不符，沒有任何讀寫

#### Scenario: 既有 demo 資料庫的升級

- **WHEN** Demo 身分首次開啟升級前的 `funding.db`
- **THEN** 資料庫被標為 `demo`、schema 版本提升，既有資料不變

#### Scenario: 舊版程式拒絕新資料庫

- **WHEN** 以只支援舊 schema 版本的程式開啟升級後的資料庫
- **THEN** 以「版本過新」拒絕開啟

### Requirement: Live 客戶端全部走 live 主機，且不建立 demo 客戶端

Live 身分下，帳戶餘額與持倉讀取、槓桿上限讀取、資金費流水、費率來源、時鐘校時與下單 SHALL 全部使用 live 主機；程式 SHALL NOT 建立任何指向 demo／testnet 主機的客戶端。Demo 身分 SHALL NOT 建立任何指向 live 主機的客戶端。每個交易所的校時偏移 SHALL 以該身分的主機量測。

#### Scenario: Live 身分只連 live 主機

- **WHEN** 以假傳輸記錄 Live 身分啟動後的所有簽名請求目標主機
- **THEN** 目標全部屬於 live 主機清單，沒有任何 demo 主機

#### Scenario: Demo 身分只連 demo 主機

- **WHEN** 以假傳輸記錄 Demo 身分啟動後的所有簽名請求目標主機
- **THEN** 目標全部屬於 demo／testnet 主機清單，沒有任何 live 主機

### Requirement: 訂單 ID 前綴包含 live

客戶端訂單 ID 的前綴 SHALL 有 `sim`、`demo`、`live` 三種；Live 身分送出的訂單 SHALL 使用 `live`。重啟對帳 SHALL 能由前綴辨識 live 訂單。新前綴 SHALL 與既有前綴等長，使 ID 總長度不超過兩所的上限（36 字元）。

#### Scenario: Live 單使用 live 前綴

- **WHEN** Live 身分的引擎建立一筆下單意圖
- **THEN** 其客戶端訂單 ID 以 `live` 前綴開頭，長度不超過 36

### Requirement: Live 身分排除 OKX

Live 身分下，引擎 SHALL 拒絕任何含 OKX 腿的配對（加入交易單與進場兩處都檢查），執行器 SHALL 只建立 Binance 與 Bybit，可交易交易所清單 SHALL 只含 Binance 與 Bybit。

#### Scenario: 含 OKX 腿的配對被拒

- **WHEN** Live 身分的引擎收到一個含 OKX 腿的配對
- **THEN** 加入交易單被拒絕，並說明 live 不支援 OKX

# 正式環境（真錢）交易：規劃 v2（已經過三鏡頭審查）

狀態：**規劃草案，尚未實作、尚未經你確認**。範圍：先打通 Binance 與 Bybit（OKX 不含）。日期：2026-10-15。
v1 經 skeptic（正確性）、red-team（安全）、simplifier（過度設計）三個獨立審查，**三個都推翻了 v1**；本版依審查結果重寫。文末「審查紀錄」列出 v1 錯在哪裡，以及哪些我親自對照程式碼確認過。

## 0. 一句話

**不要在同一個程式裡做「模擬／demo／live 三態切換」。改成「啟動時就決定、整個程式不會變」的身分：`tong-funding --live`。** 這個身分一次決定：用哪個資料庫、哪個 Keychain 項目、哪組簽名主機與傳輸、視窗標題與橫幅。程式內部沿用現有的「模擬／交易所下單」二態，不加第三態，不遷移資料，不在每個事件加環境欄位。

「武裝」直接用既有的 kill switch：live 程式**每次啟動都強制 kill switch = ON**（只擋開倉、平倉照常），要手動輸入確認字串才能關掉；出事時自動打開（＝自動解除武裝，同時仍能平倉）。

## 1. 現況（本節每一項我都對照過程式碼，修正了 v1 的錯）

| 面向 | 現況（檔案） | 對 live 的意義 |
|---|---|---|
| 簽名主機 | 編譯期寫死 demo／testnet（`signed/endpoints.rs`）；執行期還有第二道防線：傳輸層主機白名單 `HostPolicy::SignedDemo`（`reqwest_transport.rs:43-53`），公開傳輸 `PublicProduction` 只拒絕 OKX 標頭，**不拒絕** Binance／Bybit 的簽名標頭與 `signature=` | **v1 完全漏掉這一層。** 如果只把 live 主機加進 `ALLOWED_SIGNED_HOSTS`，demo 傳輸也會放行 live；而且「簽名請求誤走公開傳輸」今天只會打到 demo 主機被拒，加了 live 就變成真錢請求 |
| 限頻分類 | `classify_request` 用「主機是否在 `ALLOWED_SIGNED_HOSTS`」判斷簽名請求（`ratelimit.rs:96-105`） | live 簽名主機（`fapi.binance.com`／`api.bybit.com`）與公開行情同一台，分類會失準；且 Binance 的 IP 權重是掃幣與下單共用，掃幣被 429／418 封鎖時，**平倉也會被擋** |
| 金鑰 | 單一 Keychain 項目 `credentials`（JSON），鍵為 `{交易所}:api_key`…（`store/secrets.rs`）；`secrets import-env` 會把 `BINANCE_API_KEY` 寫到沒有前綴的欄位（`secrets_cli.rs:153-155`） | demo 與 live 在同一項目會被一次解鎖；匯入 live 的 `.env` 會寫進 demo 那一格 |
| 環境標記 | 二態：`simulated` 布林；`executor_mismatch`（`actor.rs:1276`）；`IdPrefix` 只有 Sim／Demo（`ids.rs:16`）；持久化的 `FLAG_EXECUTION_MODE` | 見 4（為什麼不加第三態） |
| 平倉與模式 | `begin_close` 開頭就檢查 `executor_mismatch`，不符就**拒絕平倉**（`actor.rs:1813-1816`）；有另一模式的未結配對時切換模式被拒（`gate.rs:151-160`）；`gate.rs:59` 用 `mode == ExchangeDemo` 判斷是否在對帳未完成時擋單 | 「解除武裝回到模擬」會讓 live 部位**沒人能平倉**；新增模式變體時這個相等比較不會報編譯錯誤 |
| 單筆金額 | 掃幣試算用常數 1,000 USDT；**實際下單名目來自使用者可改的合約模板**，儲存時只檢查 > 0（`actor.rs:2553-2558`）；手動下單頁送出 `notional_usdt: None`，數量由使用者輸入（`actor.rs:2026-2071`） | v1 說「固定 1,000」是錯的；**沒有任何金額上限**，手動下單頁完全不受約束 |
| 預設值 | 全新資料庫會用程式預設：`trigger_mode = AUTO`（`risk.rs:136`） | 新的 live 資料庫若不處理，會以 AUTO 起步 |
| 規格 | `app-shell` 規格明文「畫面不出現『LIVE』字樣」；`risk-config` 規格「`execution_mode = LIVE` 驗證失敗」 | 這兩條要明確修改（不是繞過） |
| 啟動 | 只開預設資料庫（`main.rs:44`）；`--db` 可指任意路徑（`config_cli.rs`、`import_cli.rs`） | 見 4 |
| 保證金 | Binance 完全沒檢查全倉／逐倉；Bybit 讀不到 `totalAvailableBalance` 會報錯（隱性）；OKX 逐倉會報錯 | v1 說「沒有檢查」過度簡化 |
| kill switch | 阻擋開倉（含提早平倉，`actor.rs:1005`）、平倉與取消照常（`gate.rs:66-73`）；存在資料庫的 `system_flags` | 正好就是我們要的「只平倉」語意 |

## 2. 設計（v2）

### 2.1 程式身分 = 環境
1. `--live` 啟動的程式只會碰 live：資料庫 `funding-live.db`、Keychain 項目 `tong-funding-live`、簽名傳輸只放行 live 主機。不帶 `--live` 的程式只會碰 demo／模擬，且**傳輸層拒絕 live 主機**。兩者互相不可能越界（用測試證明）。
2. 全域單一實例鎖（不分資料庫）：同一時間只能跑一個程式，避免 demo 與 live 同時跑、共用 IP 權重與 Keychain。
3. 資料庫寫入不可變的環境中繼資料（`db_venue`），開檔時與程式身分比對，不符就停機；schema 版本提升，讓舊版二進位拒絕開啟。
4. 在 live 程式裡，`SIMULATION` 仍存在：它就是「影子模式」（用 live 公開資料掃幣與做進場前檢查，不下單），不需要新增任何東西。
5. 要 live 的所有環境相關程式（帳戶讀取、時鐘校時、資金費流水、費率來源、槓桿上限、下單）都由「程式身分」決定主機，**沒有第二個入口**；移除讀環境變數選主機的做法（`TONG_FUNDING_BINANCE_HOST`）對 live 無效。

### 2.2 傳輸與主機（三條防線）
1. 新增 `HostPolicy::SignedLive` 與獨立的 `LIVE_SIGNED_HOSTS`，**絕不併入** `ALLOWED_SIGNED_HOSTS`；live 主機字串只出現在 `signed/endpoints.rs` 一個檔案，`static_checks` 只放行這個檔案，並把「主機字串掃描」擴大到整個 `src`。
2. 公開傳輸改為拒絕**所有**交易所的認證標頭（`X-MBX-APIKEY`、`X-BAPI-*`、`OK-ACCESS-*`）與 `signature`／`sign` 參數；公開、簽名（demo）、簽名（live）用不同的型別，接錯在編譯期失敗。
3. 限頻分類改為依「呼叫端型別」而非主機；平倉與查單保留權重配額；收到 418／429 → 同主機所有類別一起退避，平倉不排隊而是告警，並自動 kill switch = ON。live 的掃幣降頻，優先用 WebSocket（Binance 已有、Bybit 已有）。

### 2.3 武裝 = kill switch（引擎層，不只是畫面）
1. live 程式每次啟動（包含當機重啟）kill switch 強制為 ON。
2. 解除（開啟交易）是引擎指令 `Arm { preflight_id, caps_hash }`：`preflight_id` 必須是引擎自己在最近 N 秒內產生且全綠的預檢結果、`caps_hash` 必須等於當下儲存的上限；UI 打字確認只是前端流程。
3. 武裝時上限凍結在引擎記憶體；武裝中調高上限必須先 kill switch ON 再重新武裝，調低即時生效。設定命令列工具（`config set --db`）**不得**寫入 live 資料庫的上限。
4. 自動 kill switch = ON 的條件（MVP 只做這幾個）：任何 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED`、認證錯誤、418／429、資料庫寫入失敗、時鐘失同步。
5. 觸發模式在 live 資料庫初始化時強制為 `MANUAL`，提早平倉為 0（停用）。

### 2.4 硬上限（在引擎，不在畫面）
1. **每腿名目上限**：在 `OrderRequest` 建好之後用「數量 × 最差保護價」檢查所有**非 reduce-only** 單（含配對進場、補單），也在儲存合約模板時檢查；超過就拒絕，進位後超過寧可少下。
2. **手動下單頁：live 時停用開倉**（只留 reduce-only 平倉與取消），並提供專用的「平掉單腿」動作給 `PARTIAL_FAILURE` 使用（使用者 2026-10-16 同意）。**另外保留「前往交易所」連結**：橫幅與每個 `PARTIAL_FAILURE`／`IMBALANCED` 配對卡上提供按鈕，用預設瀏覽器開啟該幣在 Binance 合約與 Bybit 合約的網頁交易介面（只是開網址，不帶任何金鑰），讓使用者可以直接到交易所手動平倉（實作時再核對各交易所深層連結格式）。
2a. **每次武裝的配對數上限**（使用者要求「最多先 3 筆」）：新增 `live_max_pairs_per_arming = 2`，一次武裝期間累計進場滿 2 個配對就自動 kill switch = ON（只能平倉），要再交易必須重新武裝；階段 2 同時配對數沿用 `max_concurrent_pairs = 1`。
3. 同時配對數沿用既有 `max_concurrent_pairs`（live 設 1），不另建設定。
4. **最大的真正硬上限在交易所端：開 live 專用子帳戶，只放小額保證金（階段 2：每所 30 USDT、每腿名目 10 USDT），關閉提領，設 IP 白名單。** 子帳戶內使用全倉即可（損失上限就是子帳戶餘額）；Bybit 必須用全倉（`REGULAR_MARGIN`），因為目前程式讀取帳戶時遇到逐倉帳戶模式（讀不到 `totalAvailableBalance`）會報錯。 這是 app 出任何 bug 都繞不過的上限（審查估算：繞過所有軟體上限時，最壞損失是「存入金額 × 交易所最高槓桿」，也就是整筆存款）。
5. 引擎拒絕任何含 OKX 腿的配對（掃幣與執行器目前仍把 OKX 列為可交易，且 OKX 的 demo／正式共用主機）。

### 2.5 重啟與當機
1. live 程式啟動時載入 `funding-live.db` 的未結配對，以 live 金鑰做**唯讀對帳**；**不自動武裝**（kill switch 維持 ON）；未結部位以全螢幕紅色區塊顯示。
2. 因為程式身分固定是 live，重啟後執行器仍是 live，`T+15` 與人工平倉可以正常送出；不會出現「回到模擬導致平不掉」。
3. 資料庫寫入失敗會讓平倉也送不出（平倉採先落地事件再動作）。MVP：預檢加磁碟空間檢查；停機時發 macOS 通知與全螢幕警示並附「到交易所手動平倉」步驟；僅 reduce-only 的緊急通道列為後續項目。

### 2.6 非常顯著的提醒（由程式身分產生，不由布林產生）
1. 每一頁頂端固定一條不可關閉的紅色橫幅：`⚠ LIVE 真錢 · Binance + Bybit 正式環境`；視窗標題 `[LIVE]`；一定寫出文字，不靠顏色。demo 為琥珀色 `DEMO`，模擬 `模擬`。
2. 每個「加入交易單」「進場」「平倉」「立即平倉」的確認對話框重複顯示環境與金額；live 的按鈕文字為「以真錢下單」。
3. kill switch 的狀態在橫幅上顯示：`交易已鎖定（只能平倉）`／`交易已武裝`。
4. 修正所有把「非模擬」一律寫成 `EXCHANGE_DEMO` 的地方：系統通知（`engine/alert.rs:143`）、手動下單頁的帳本來源（`ui/vm/manual_order.rs:396-398`）、模式按鈕的啟用條件（`trading_pages.rs:892`），文案一律由程式身分產生。
5. 修改 `app-shell` 規格的「不出現 LIVE 字樣」為「live 程式必須顯示 LIVE；demo／模擬程式不得顯示」。

### 2.7 金鑰
1. live 金鑰存在獨立的 Keychain 項目（service `tong-funding-live`），只有 `--live` 程式會讀；`secrets import-env --live <檔案>` 只讀 `LIVE_*` 與無前綴名稱並寫入 live 項目（使用者後來決定允許；有衝突、OKX、與 demo 相同值的防護，見 `secrets-import-live`），不帶 `--live` 則只寫 demo。
2. 預檢（MVP）：用 live 主機讀一次餘額與持倉（失敗即封閉，也就證明不是 demo 金鑰）；其餘列為**人工勾選清單**（提領已關閉、IP 白名單、單向持倉、逐倉、子帳戶限額、手續費率從交易所頁面抄下並手動填入設定）。
3. 金鑰指紋只顯示 API key 的末 4 碼，不取自 secret，且**不寫入事件**；預檢結果只存布林摘要，不存交易所原始回應（Bybit 的回應含使用者 IP）。

## 3. 全流程檢視（live 要注意的）

| 步驟 | live 風險 | 處置 |
|---|---|---|
| 掃幣／達標 | 已用正式資料，同步結算規則已擋單邊結算 | 手續費改為手動填入實際費率；滑價預留 0.005%／腿 與實測不符，用階段 2 的資料校準 |
| 加入交易單 | 合約模板可被改成任意名目 | 模板儲存與送單前都檢查上限（2.4） |
| 進場前檢查（T−10 秒） | 帳戶讀取若仍走 demo 主機會判斷錯誤 | 所有帳戶／時鐘／流水客戶端由程式身分決定主機；時鐘每個環境各自校時 |
| 下單 | 市價單滑價是真錢損失；觸發到回應 1.3～2.4 秒；**成交可能落在結算之後**（領不到資金費卻付了手續費） | MVP 維持市價單＋既有價格漂移檢查＋極小額；把成交逾時縮到結算前並標記晚於結算的成交；**IOC 限價單不採用**（兩腿各自 IOC 常見結果是一腿全成交、另一腿 0，直接製造單腿暴露） |
| 成交監控 | 一腿成交另一腿失敗 → 人工處理；跨保證金時裸腿虧損不受名目限制 | 逐倉為預檢人工勾選的硬條件；專用「平掉單腿」動作；AUTO 之前必須定義自動處置規則與最長裸露時間 |
| 持倉／提早平倉 | 提早平倉會留下單邊曝險 | live 初始為 0（停用），小額階段通過後才開 |
| 平倉 | 時鐘需有效；IP 權重被掃幣用完 | 平倉前時鐘未同步就先重新校時；保留平倉權重（2.2） |
| 資金費流水 | Bybit 曾延遲 3 小時、損益 `INCOMPLETE` | 未收到流水不當作最終獲利；與 T−10 推算對帳；日損上限用保守估計（AUTO 前才做） |
| 損益／對帳 | 手動每日用 `issue/tools/trade_table.py` 核對 | AUTO 前補自動對帳（交易所權益快照差） |
| 重啟 | 見 2.5 | — |

## 4. 為什麼不是三態 `Venue`（v1）
- 三態要改 `ExecutionMode`、`PairEnvelope`、UI、gate、大量測試；而同一個程式裡混用 demo 與 live 本來就該禁止。
- 審查指出的相等比較（`gate.rs:59`、`leverage_cap.rs:59/98`、`actor.rs:536/654/2376/2476`）在新增變體時不會報編譯錯誤，會靜默在 live 放行；程式身分方案不新增變體，這整類風險不存在。
- 「重啟回到模擬」「解除武裝回到模擬」在現有架構下平不掉 live 部位；程式身分方案沒有這個問題。

## 5. OpenSpec 變更（從 5 個縮成 2 個，加 1 個延後）

| 變更 | 內容 | 預估任務數 |
|---|---|---|
| A `live-process` | 程式身分（`--live`）、單一實例鎖、資料庫中繼資料與 schema 版本、Keychain 項目、`SignedLive` 傳輸政策與型別、公開傳輸拒絕簽名標頭、限頻分類改依型別、`IdPrefix::Live`、帳戶／時鐘／流水依身分選主機、橫幅與標題與文案、拒絕 OKX 腿、修改 `app-shell` 與 `risk-config` 規格、對應的主機掃描 | 約 10～12（若超過再拆） |
| B `live-guardrails` | 啟動強制 kill switch、`Arm` 指令與預檢、上限凍結、每腿名目硬上限（含合約模板與手動單）、live 停用手動開倉、自動 kill switch 條件、config CLI 不得寫 live 上限、live 資料庫初始化為 MANUAL | 約 8～10 |
| C（延後，開 AUTO 之前）`live-auto-readiness` | 日損上限（保守估計）、自動對帳、手續費 API、金鑰權限 API、macOS 通知、緊急平倉通道、成交晚於結算的處理 | 之後再議 |

順序：A → 階段 0 → B → 階段 2；C 在開 AUTO 之前。

## 6. 上線階段

| 階段 | 內容 | 通過條件 |
|---|---|---|
| 0 唯讀 | live 程式啟動（kill switch ON）：讀餘額、持倉、流水；不下單 | 兩所都讀得到；用測試證明 kill switch ON 時不會發出任何簽名的寫入請求；demo 程式傳輸拒絕 live 主機（紅轉綠） |
| 1 影子 | live 程式的 `SIMULATION`：live 掃幣與進場檢查，只記錄 | 連續數個結算點，預期損益與事後以公開資料重算一致 |
| 2 極小額人工 | 子帳戶、全倉、**每腿 10 USDT、每所存 30 USDT、同時 1 個配對、一次武裝最多 2 筆（使用者 2026-10-16 改為 2；最小名目不夠時可把每所存款加碼到 50 USDT，槓桿固定 5 倍）**、`MANUAL`、提早平倉 0（使用者 2026-10-16 拍板） | 單筆損益、手續費、滑價、資金費與預期對得上；無 `PARTIAL_FAILURE`；流水與帳戶餘額一致 |
| 3 小額自動 | 先完成 C，再開 AUTO | 多個結算點無異常；自動 kill switch 經演練可用 |
| 4 放大 | 逐步提高子帳戶存款與上限 | 每次提高前回顧損益、滑價、延遲 |

## 7. 已決定（2026-10-16，使用者）與待辦
1. **同意**：`--live` 另開程式，取代程式內切換。
2. **階段 2 數字**：每腿名目 10 USDT、每所子帳戶存 30 USDT、一次武裝最多 2 筆（使用者 2026-10-16 改為 2；最小名目不夠時可把每所存款加碼到 50 USDT，槓桿固定 5 倍）（見 2.4 的 2a）。
3. **手動下單頁**：live 時停用開倉，保留平倉與取消，並提供「前往交易所」連結。
4. **帳戶型態**：使用者還沒有正式合約帳戶；請依第 9 節的設定指南準備（子帳戶、單向持倉、全倉、關閉提領、IP 白名單），並回報確認結果（不要貼金鑰）。

## 9. 正式合約帳戶設定指南（階段 0 之前使用者要做的事）

> **資料來源與限制：** 以下來自 Binance 與 Bybit 官方 API 文件與搜尋到的操作指南。我沒有你的帳戶、也沒有登入交易所，**畫面上的按鈕名稱與流程以你實際看到的為準**；標示「待確認」的是我沒有官方文件佐證的。

### 9.1 目標結構（滿足「分倉」）
- 每所各開一個**專用子帳戶**（只放要測試的小額資金，與你其他資產隔離）。
- 產品：Binance 的 **USDⓈ-M 永續合約**、Bybit 的 **USDT 永續（linear）**（統一帳戶 UTA 內）。
- 持倉模式：**單向（One-way）**；保證金：**全倉（Cross）**，因為損失上限是子帳戶餘額；Bybit **不要**用逐倉帳戶模式或組合保證金（程式目前不支援）。
- 子帳戶的 API 金鑰：只開合約交易，**不開提領**，**設定 IP 白名單**。

### 9.2 Binance
1. 主帳戶完成身分驗證，並先開通 USDⓈ-M 合約。若是第一次使用子帳戶功能，要先在網頁端啟用子帳戶功能。
2. 建立子帳戶（網頁：Sub Accounts → Create Sub Account）；建立後在該子帳戶的操作選單啟用 Futures（API：`POST /sapi/v1/sub-account/futures/enable`）。第三方指南提醒「建立時沒開合約，之後可能開不了」——**待確認**，建議建立時就開。
3. 轉入 30 USDT：主帳戶 → 子帳戶，再把資金放到子帳戶的 USDⓈ-M 合約錢包。
4. 為子帳戶建立 API 金鑰（API Management → 選子帳戶 → Create API，System generated）：先設定「只允許受信任 IP」並填入你的對外 IP，再勾 **Enable Futures**，**不要勾提領**。一個子帳戶最多 30 組金鑰。
5. 持倉模式：官方文件寫明 `dualSidePosition=false` 為單向、設定套用到**所有**交易對，且有未平倉部位或掛單時改不了（錯誤 `-4067`／`-4068`）；請在**沒有部位時**確認是單向（合約介面偏好設定）。
6. 保證金類型：官方 `marginType` 是逐交易對設定（ISOLATED／CROSSED）；**子帳戶內用全倉即可**，程式不檢查，也不需要改。不要啟用 Multi-Assets Mode 與 Portfolio Margin（程式假設單一 USDT 保證金；**待確認** Multi-Assets 對下單的影響）。
7. 槓桿：由程式依配對下單前逐幣設定（最大 5 倍，見風控設定），你不用事先設定。

### 9.3 Bybit
1. 主帳戶完成身分驗證。建立子帳戶時選 **Standard 子帳戶 + Unified Trading Account（UTA）**；建立 API 金鑰可能要求該子帳戶先綁 2FA。
2. 轉入 30 USDT（主帳戶 → 子帳戶，Assets → Transfer）。
3. 為子帳戶建立 API 金鑰：System-generated、API Transaction、**Read-Write**，**IP 白名單**；權限勾**合約交易（Orders、Positions）**，**不勾提領／轉帳**。
4. 保證金模式：維持預設**全倉（REGULAR_MARGIN）**。官方 `POST /v5/account/set-margin-mode` 的值有 `ISOLATED_MARGIN`／`REGULAR_MARGIN`／`PORTFOLIO_MARGIN`，預設為 regular；**不要改成逐倉或組合保證金**，否則程式讀帳戶會報錯（`bybit.rs:326`）。
5. 持倉模式：官方文件寫明 UTA 的 USDT 永續預設為**單向（mode 0）**，與程式的 `positionIdx: 0` 一致；有部位或掛單時不能切換。

### 9.4 兩所共通
- 你的對外 IP 如果會變（家用網路常見），白名單會讓程式突然失效，這是安全的失敗（請求被拒），但要知道。
- 金鑰**只放進 live 程式的 Keychain 項目**（變更 A 完成後提供指令），不要貼到對話、不要放進 `.env`。
- 我需要你回報的（**不含任何金鑰或完整帳號**）：兩所子帳戶的「持倉模式」「保證金模式」「合約錢包餘額」畫面截圖，帳號與金鑰碼請遮蔽。

### 9.5 10 USDT 夠不夠？（我查了兩所公開規則，今天的數字）
| 幣 | Binance 最小名目 | Bybit 最小名目 | 最小一單約 |
|---|---|---|---|
| BLUR、WAL、SIREN、ARC、1000CAT、DOGE | 5 USDT | 5 USDT | 0.00～0.08 USDT（遠小於 10） |
| RSR | 5 USDT | 5 USDT；Bybit 步進 10 顆 | 約 0.02 USDT |
| ETH | 20 USDT | 5 USDT；Bybit 最小 0.01 顆 | Bybit 約 24 USDT（**超過 10，會被拒**） |
| BTC | 50 USDT | 5 USDT | 約 82 USDT（**超過 10，會被拒**） |

結論：10 USDT 對掃幣常出現的小幣足夠；BTC、ETH 這類高單價幣在 10 USDT 下單會被交易所拒絕（引擎會因下單失敗而取消配對，不會造成單腿，但**要在實作時補「最小名目」的送單前檢查**，避免白白打 API）。每腿 10 USDT、5 倍槓桿，保證金約 2 USDT，兩所各 30 USDT 綽綽有餘；資金費收益與手續費都在幾分錢等級，這一階段的目的是驗證流程，不是賺錢。

## 8. 審查紀錄
- **三個審查員都判 v1「REFUTED」**（skeptic、red-team、simplifier）。依專案規則：三鏡頭過半存活才算採信；v1 沒有任何一個鏡頭認可，故重寫。
- **我親自對照程式碼確認過的審查結論**：`begin_close` 在環境不符時拒絕平倉；`gate.rs` 禁止在有另一模式未結配對時切換；`gate.rs:59` 的相等比較；`HostPolicy` 的兩個政策與 `classify_request` 共用同一份主機清單；`IdPrefix` 只有 Sim／Demo；合約模板只檢查 > 0；手動下單 `notional_usdt: None`；`app-shell` 規格的「不出現 LIVE」；`main.rs` 只開預設資料庫。
- **審查員提到、我尚未逐一核對**：`leverage_cap.rs`、`actor.rs:536/654/2376/2476` 的相等比較（本版方案不新增變體，影響不大）、`engine/alert.rs:143`、`manual_order.rs:396`、`recovery.rs:1002` 的行為、Binance 市價單 `newClientOrderId` 的唯一性語意。這些在實作變更 A 時會逐一核對並補測試。
- **所有交易所端點與錯誤碼（金鑰權限、費率、`apiTradingStatus` 語意、Bybit 認證錯誤碼）都沒有網路驗證**，審查員也只憑記憶；MVP 已避免依賴它們（改為人工清單）。

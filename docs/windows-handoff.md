# Windows 移植交接文件（給在 Windows 上接手的 Claude agent）

> 建立：2026-10-19，由 Mac 上的 Claude session 撰寫。
> 狀態：**只有調查，沒有任何程式修改。** 下面「證據」一節的每一條都是在 Mac 上用 grep 讀程式得到的，**沒有在 Windows 上編譯或執行過任何東西**。標「未驗證」的地方，請你在 Windows 上實測後再相信。
> 讀者：在使用者（Eason）的 Windows 家用機上 clone 此儲存庫的 Claude agent。使用者沒有裝 Rust。

## 0. 先讀這幾條（硬性規則）

1. **範圍只到 demo。** 這份文件的目標是「Windows 上能編譯、能跑 demo、測試綠」。**不要在 Windows 上輸入或匯入真錢（`--live`）金鑰，不要武裝。** Windows 跑 live 是另一個決定，使用者尚未做。
2. **走 OpenSpec 流程。** 本專案有 `openspec/`。這是新功能（跨平台移植），請先 `openspec new change windows-port`，產出 proposal／design／tasks，**等使用者確認 proposal 後才開始改程式**。單純的環境安裝與只讀調查不需要等。
3. **不要放寬任何保護。** 程式有刻意的 fail-closed 設計（單一實例鎖、資料庫權限、live 主機白名單、`app/src/exchange/static_checks.rs` 的原始碼掃描測試）。移植時如果某個保護在 Windows 上做不出來，**要明說「此平台缺這個保護」並停下來問使用者，不要默默拿掉或改成永遠通過。**
4. **金鑰不進對話、不進 git。** 不要要求使用者貼金鑰。`.env-*` 已被 `.gitignore` 忽略。
5. **證據優先。** 改邏輯要有 fail→pass 的測試證據；回報時附上實際執行的指令與測試檔路徑。沒跑過就說「已修改、未驗證」。
6. **使用者偏好**：用繁體中文回覆；第一句先講結果；需要使用者動手時用 STAR（情境、任務、行動、結果）說明；使用者自己負責合併到 main，不要替他合併或推送。

## 1. STAR

### Situation（情境）
- 專案是 Rust workspace：`app`（套件 `tong-funding`，GPUI 圖形介面）與 `core`（`tong-funding-core`，純邏輯）。用於 Binance／Bybit 的資金費率套利，已有 `--live` 真錢模式（在 Mac 上，尚未對真實交易所下過實單）。
- 目前**只在 macOS 開發與測試**（約 1959 個測試在 Mac 全綠）。
- 使用者有一台閒置的 Windows 家用機：**Windows Home 版、走 Wi-Fi、尚未裝 Rust／Visual Studio Build Tools／Git、未登入也未更新過**。使用者最後想從 Mac 透過 Tailscale 遠端使用它，但這個階段先不討論真錢。
- 程式碼目前**無法在 Windows 編譯**：有多處直接使用 Unix 專用 API 與 macOS 鑰匙圈（見第 2 節）。

### Task（任務）
依序達成，每一階段都要有可重現的指令與輸出：

| 階段 | 目標 | 完成條件 |
|---|---|---|
| W0 | Windows 開發環境就緒 | `rustc --version`、`cargo --version`、`git --version` 都能在新開的 PowerShell 執行；`cargo build -p tong-funding-core` 成功 |
| W1 | `core` 在 Windows 通過測試 | `cargo test -p tong-funding-core` 全綠（`core` 沒有平台專用程式，預期容易） |
| W2 | `app` 在 Windows 能編譯 | `cargo build -p tong-funding` 成功（需要先完成第 3 節的移植項目 P1–P5） |
| W3 | `app` 測試在 Windows 的結果被分類 | `cargo test -p tong-funding` 的失敗逐一分類為：該平台不適用而加上 `cfg`、真的移植缺陷並修正、或環境問題。**不可用刪除測試或放寬斷言來讓它變綠** |
| W4 | demo 圖形介面能開起來 | `cargo run -p tong-funding` 開出視窗，總覽頁可見，繁體中文不是方塊字；使用者用眼睛確認並回報 |
| W5 | 金鑰存取可用 | 在 Windows 憑證管理員中 `secrets set` → `secrets status` 能讀回「已設定」（只用 demo 的測試金鑰，不用 `--live`） |
| W6 | 自動編譯（可選） | GitHub Actions 的 Windows 工作可編譯並上傳 `.exe`（不跑真錢） |

### Action（行動）
見第 4 節，依序操作。

### Result（預期結果與回報）
- 一份 PR／分支，內含：OpenSpec change `windows-port`、移植程式碼、測試、更新後的 `docs/`（Windows 操作說明）。
- 回報格式見第 6 節。

## 2. 證據：目前阻擋 Windows 的地方（Mac 上 grep 所得，未在 Windows 驗證）

| # | 位置 | 問題 | 影響 |
|---|---|---|---|
| E1 | `app/Cargo.toml`：`keyring = { version = "3", features = ["apple-native"] }` | 只啟用 macOS 後端 | Windows 沒有金鑰儲存後端（keyring v3 另有 `windows-native` 功能，**未驗證能否直接用**） |
| E2 | `app/Cargo.toml`：`libc = "0.2"`；`app/src/store/db.rs:634-646` `free_disk_bytes` 用 `libc::statvfs` 與 `std::os::unix::ffi::OsStrExt` | Windows 沒有 `statvfs` | 編譯失敗；這是 live 預檢的「磁碟空間」檢查 |
| E3 | `app/src/instance_lock.rs:16,60,76`：`std::os::unix::fs::{DirBuilderExt, OpenOptionsExt}`，目錄 `mode(0o700)`、檔案 `mode(0o600)` | Unix 專用 API | 編譯失敗。**但**鎖本身用的是標準函式庫的 `File::try_lock`，應可跨平台（未驗證 Windows 行為） |
| E4 | `app/src/instance_lock.rs:91-94,106`、`app/src/store/db.rs:150,598-609`、`app/src/funding/live_slot_probe.rs:51` | 資料目錄寫死為 `$HOME/Library/Application Support/tong-funding`，且讀 `HOME` 環境變數 | Windows 沒有 `HOME`（用 `USERPROFILE`／`%APPDATA%`），啟動會直接失敗（程式設計為 fail-closed） |
| E5 | `app/src/store/db.rs:20,173-175,224-226,263-283` 等：`PermissionsExt`、`Permissions::from_mode`、`real_chmod`；啟動時檢查並收緊資料庫目錄／檔案權限 | Unix 權限位元 | 編譯失敗；Windows 需決定等價做法（例如只依賴使用者設定檔目錄的 ACL，並**明說**） |
| E6 | `app/src/store/secrets_cli.rs:335-341` `warn_if_readable_by_others` 用 `PermissionsExt` | Unix 專用 | 編譯失敗；Windows 可改成不提示並在說明文件註明 |
| E7 | 測試：`instance_lock.rs:116,161,171`（`PermissionsExt`、`symlink`）、`store/import_cli.rs:237`、`db.rs` 內測試（`from_mode` 等） | Unix 專用 | 測試編譯失敗；需 `#[cfg(unix)]` 或改寫 |
| E8 | `app/src/engine/alert.rs:14` 註解提到 macOS 的 osascript 通知 | 目前只是註解（尚未實作系統通知） | 無影響，但別假設有通知 |
| E9 | `tools/*.swift`、`tools/window_shot.sh` | macOS 開發輔助工具（截圖／點擊） | Windows 不需要，忽略 |
| E10 | `app/src/ui/fonts.rs` 內嵌 IBMPlexMono（`app/assets/fonts/*.ttf`） | 只內嵌等寬英數字型 | **中文字型靠系統**；Windows 預設會不會顯示成方塊字**未驗證**（W4 要看） |
| E11 | GUI 相依：`gpui-kit = "=0.7.1"`（來自 crates.io；`Cargo.lock` 無 git 相依）。`gpui-base-0.7.1` 的 Cargo.toml 有 `target.'cfg(target_os = "windows")'.dependencies.windows` | 顯示有 Windows 相依宣告 | 這只代表「有人考慮過 Windows」，**能否真的編譯、能否開出視窗未驗證**，是整件事最大的未知 |

另有：沒有 `.gitattributes`（換行字元風險，見 4.0）、沒有 `.github/`（沒有 CI）。

### 沒有發現的東西（避免你白找）
- 程式碼裡沒有 `cfg(debug_assertions)` 之類的 debug／release 分支。
- 沒有 git 相依（`Cargo.lock` 的 `source = "git+` 數量為 0），不需要 Git 憑證才能拉相依。

## 3. 建議的移植項目（P1–P5，仍須先經 OpenSpec 確認）

這些是「建議」，不是已決定的設計。每項都先寫失敗測試或至少寫可重現的編譯錯誤，再修。

- **P1 金鑰儲存**：把 `keyring` 改成「依平台選後端」的目標相依（macOS 保持 `apple-native`，Windows 用 `windows-native`）。驗收：W5。Keychain service 名稱（`tong-funding`／`tong-funding-live`）維持不變。
- **P2 資料目錄**：抽出單一函式回傳資料目錄（macOS 維持 `~/Library/Application Support/tong-funding`；Windows 用 `%APPDATA%\tong-funding`）。`instance_lock.rs`、`db.rs`、`live_slot_probe.rs` 都改用它，並移除「HOME 未設定」的假設。**macOS 的路徑不能變**，否則既有資料庫會「消失」。
- **P3 單一實例鎖**：保留 `File::try_lock`，把 `mode(0o700/0o600)` 與 `symlink_metadata` 的檢查用 `cfg(unix)` 包起來；Windows 上的等價防護（例如拒絕重新解析點）要寫明有沒有做。驗收：新增「第二個實例被拒絕」的測試在 Windows 通過（可用現有 `instance_lock.rs` 測試改寫）。
- **P4 權限檢查**：`db.rs` 的目錄／檔案權限收緊與 `secrets_cli.rs` 的警告，Unix 行為不變，Windows 提供明確的替代或明確的「不適用」。**如果決定不適用，必須在 `docs/` 與啟動訊息裡說出來**，不能靜默。
- **P5 磁碟空間**：`free_disk_bytes` 在 Windows 用對應的系統呼叫（例如 `GetDiskFreeSpaceExW`，可經 `windows` 或其他已審查的 crate）。注意 `static_checks.rs` 會掃描原始碼，新增相依或字面字串前先跑它，看有沒有被規則擋下，**不要為了通過而改規則**，有疑問先問使用者。

不在範圍：任何交易邏輯、交易所適配器、風控與預檢規則的行為改動。

## 4. 操作步驟

### 4.0 使用者先做（Windows，PowerShell；請 agent 逐步帶著做）
> 若指令無法執行，把**錯誤訊息**（不含金鑰）貼回，不要猜。

1. 查系統資訊（回報給 agent 決定下一步）：
   ```
   winver
   systeminfo | findstr /B /C:"OS Name" /C:"OS Version" /C:"System Type"
   Get-CimInstance Win32_VideoController | Select Name, DriverVersion
   ```
2. 安裝 **Git for Windows**（https://git-scm.com），安裝時換行字元選「Checkout as-is, commit Unix-style（input）」或之後執行 `git config --global core.autocrlf input`。
3. 安裝 **Visual Studio Build Tools**，勾選「使用 C++ 的桌面開發」（含 MSVC 與 Windows SDK）。`rusqlite` 用 `bundled` 會編譯 C 程式，需要這個。
4. 安裝 **rustup**（https://rustup.rs），選預設（stable，MSVC 目標）。**安裝完關掉 PowerShell 重開**。
5. 驗證：
   ```
   rustc --version
   cargo --version
   git --version
   ```
6. 取得程式碼：
   ```
   git clone <使用者提供的 GitHub 網址> tong-funding
   cd tong-funding
   git checkout <使用者指定的分支>
   ```
7. 路徑過長時（Windows 預設 260 字元限制）編譯可能失敗；若出現，以系統管理員執行 `git config --system core.longpaths true`，並把專案放在短路徑如 `C:\src\tong-funding`。

### 4.1 W0／W1：基線
```
cargo build -p tong-funding-core
cargo test  -p tong-funding-core
```
預期綠。若紅，先分類為環境問題或程式問題，再回報。

### 4.2 W2 之前：記錄「現狀編譯錯誤」
```
cargo build -p tong-funding 2>&1 | Tee-Object -FilePath build_baseline.txt
```
把錯誤摘要（不是全文）存成 OpenSpec 的證據。這是 fail→pass 中的 fail。**注意**：第一次編譯會很久（GPUI 相依多），這是正常的；請用 `run_in_background` 之類的方式等，不要反覆輪詢。

### 4.3 OpenSpec
```
openspec new change windows-port
```
產出 proposal／design／tasks，內容至少涵蓋第 3 節五項、各自的驗收與「不在範圍」。**交給使用者確認後**才 apply。tasks 若超過約 12 項，拆成兩個 change（使用者的全域規則）。實作前先 `openspec validate`。

### 4.4 實作順序
P2（資料目錄）→ P3（鎖）→ P4（權限）→ P5（磁碟）→ P1（金鑰）。理由：前四項解決編譯，P1 要等能編譯後才能實測。每完成一項：
```
cargo build -p tong-funding
cargo test  -p tong-funding <相關測試名稱>
```
並確認 macOS 行為沒變（見 4.6）。

### 4.5 W3／W4：完整測試與開窗
```
cargo test -p tong-funding 2>&1 | Tee-Object -FilePath test_windows.txt
cargo run  -p tong-funding
```
- 測試失敗請建成表格：測試名稱／失敗訊息／分類（平台不適用／真缺陷／環境）／處置。
- 開窗後請使用者回報：視窗是否出現、中文是否正常、總覽頁是否載入（沒有金鑰時顯示「未連線」是正常的）。**你自己看不到畫面**，不要宣稱「介面正常」。
- 單一實例鎖：開著一個，再開第二個，應該被拒絕並顯示雙語訊息。

### 4.6 不破壞 macOS（重要）
Windows 上的 agent 無法跑 macOS 測試。因此：
- 修改一律用 `cfg(unix)`／`cfg(windows)` 或平台目標相依，**不改變 macOS 路徑與行為**。
- 請使用者回到 Mac 後執行 `cargo test -p tong-funding`（過去結果：1959 通過、0 失敗）並貼結果，或由 GitHub Actions 的 macOS 工作代跑。
- 這是合併前的必要條件，**不要在 Mac 結果出來前說「移植完成」**。

### 4.7 W5：金鑰（只用 demo 測試金鑰）
```
cargo run -p tong-funding -- secrets set binance api-key
cargo run -p tong-funding -- secrets status
cargo run -p tong-funding -- secrets import-env .env-demo
```
`secrets status` 只顯示已設定／未設定。**不要加 `--live`。** 金鑰請使用者自己輸入。`import-env` 對未加前綴的名稱會歸到 demo。

### 4.8 W6：GitHub Actions（可選，使用者尚未要求）
- GitHub Actions 是在 **GitHub 的雲端虛擬機**上編譯，不是使用者的電腦。
- 建議工作：`windows-latest` 與 `macos-latest` 各跑 `cargo build` 與 `cargo test`；Windows 工作上傳 `.exe`。
- 私有儲存庫有免費額度限制，macOS 較耗額度；**這部分沒有查過使用者的方案**。
- 未簽章的 `.exe` 會被 SmartScreen 警告，使用者自用可「仍要執行」。

## 5. 風險與未知（請照實更新，不要粉飾）

1. **GPUI 能否在這台 Windows 開窗**：最大未知（E11）。可能需要特定 DirectX 支援／驅動；若失敗，回報錯誤訊息與顯示卡資訊，不要擅自換 UI 框架。
2. **遠端操作**：Windows Home 版沒有內建 RDP 主機功能；使用者計畫經 Tailscale 搭配遠端桌面類工具。本程式需要**已登入的桌面工作階段**才會執行。此部分不在 W0–W6 範圍，之後再談。
3. **家用網路**：Wi-Fi 與浮動 IP 會讓 API 白名單失效、斷線時可能一腿成交一腿失敗。**這是真錢階段才要處理的事**，demo 階段不影響。
4. **每次重開機都是鎖定狀態**（刻意設計），無人值守不等於自動恢復交易。
5. **Windows 權限模型不同**（P4）：資料庫檔不是 0600；是否有等價保護需要使用者決定。
6. **測試中依賴 Unix 的部分**（E7）可能不只清單列出的幾處；以實際編譯為準。

## 6. 回報格式（每個階段結束時）

- 第一句：結果（綠／紅／卡住）。
- 實際執行的指令（可複製）。
- 測試或輸出檔的路徑，讓使用者可以重現。
- 失敗照貼摘要，不要說「應該可以」。
- 下一步與需要使用者決定的事（用 STAR 說明）。
- 有畫面可看的項目，主動請使用者用眼睛確認並回報。

## 7. 完成定義

- W0–W5 的完成條件都有指令與輸出佐證。
- `cargo test -p tong-funding-core` 與 `-p tong-funding` 在 Windows 的結果有分類紀錄，沒有被刪除或放寬的測試。
- macOS 測試結果（使用者回報或 CI）與移植前相同。
- `docs/` 有 Windows 操作說明，且明講哪些保護在 Windows 與 macOS 不同。
- 使用者已確認可合併；**合併由使用者自己做。**

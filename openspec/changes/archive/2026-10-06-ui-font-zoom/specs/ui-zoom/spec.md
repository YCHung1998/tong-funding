## ADDED Requirements

### Requirement: 快捷鍵縮放整個介面

系統 SHALL 提供全域快捷鍵：`Cmd` 或 `Ctrl` 加 `=` / `+` 放大一步、加 `-` 縮小一步、加 `0` 還原為 100%。
縮放 SHALL 同時作用於所有頁面的文字、間距與固定寬度（側邊欄、輸入框），等比例改變；顏色與版面結構 SHALL 不變。
縮放比例 SHALL 介於 70% 與 200% 之間，每步 10%；已達上下限時再按 SHALL 維持不變。

#### Scenario: 放大一步

- **WHEN** 目前縮放為 100%，使用者按下 `Cmd` + `=`
- **THEN** 縮放變為 110%，內文字級由 11 變為 12.1

#### Scenario: 上限

- **WHEN** 目前縮放為 200%，使用者按下 `Ctrl` + `=`
- **THEN** 縮放維持 200%

#### Scenario: 還原

- **WHEN** 目前縮放為 70%，使用者按下 `Cmd` + `0`
- **THEN** 縮放變為 100%

### Requirement: 縮放偏好持久化

縮放比例改變後 SHALL 寫入 `config` 表的 `ui_prefs`，下次啟動 SHALL 以該比例開啟。
`ui_prefs` 不存在、無法解析或超出範圍時 SHALL 以 100% 開啟，且 SHALL NOT 阻止程式啟動。
寫入失敗 SHALL 不影響當下的縮放，只在系統日誌記錄一筆警告。

#### Scenario: 重啟後還原

- **WHEN** 使用者把縮放調到 130% 後關閉程式並重新開啟
- **THEN** 介面以 130% 開啟

#### Scenario: 設定損壞

- **WHEN** `ui_prefs` 內容為 `{"zoom_pct": 999}`
- **THEN** 介面以 100% 開啟

### Requirement: 狀態列顯示縮放比例

縮放不是 100% 時，狀態列 SHALL 顯示目前比例（例如「縮放 120%」）；為 100% 時 SHALL 不顯示。

#### Scenario: 非預設縮放

- **WHEN** 縮放為 120%
- **THEN** 狀態列出現「縮放 120%」

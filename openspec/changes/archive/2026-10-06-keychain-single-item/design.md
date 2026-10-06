## Context

- 現況：`KeychainStore`（`secrets.rs`）每次 `get` 都 `keyring::Entry::new(service, account)` 讀一個項目，account 為 `Exchange:name`；`KeychainSecrets<S: KeyStore>` 實作 `SecretProvider`，無快取。
- `live.rs` 建立一個共用的 `Arc<dyn SecretProvider>`；啟動時 `demo_keys()` 依序讀 Binance key/secret、Bybit key/secret（4 個項目 → 4 次詢問），之後帳戶輪詢、funding 迴圈、執行器建構都會再讀。
- keyring 3（apple-native）每個項目有各自的 ACL，無法一次授權多項；ACL 綁定 binary 簽章，開發版每次重新編譯都會重新詢問。
- 測試替身已齊全：`KeyStore` 有 `MemStore`（含 `failing()`）、`SecretProvider` 有 `MemorySecrets`。

## Goals / Non-Goals

**Goals:** 每次啟動最多 1 次詢問；既有使用者無痛遷移；拒絕時不轟炸詢問。

**Non-Goals:** 簽署 binary 讓「永遠允許」跨重新編譯有效；改用其他密鑰存放方式；自動刪除舊項目。

## Decisions

1. **bundle 放在 `KeyStore` 之上**：新增 `BundleSecrets<S: KeyStore>`，以 account `credentials` 存 JSON（`BTreeMap<String, String>`）。`KeyStore` 介面不變，既有 `KeychainStore` / `MemStore` 直接可用。
   - 替代方案：只加快取、不合併 → 第一次仍要 4 次詢問；捨棄。
2. **快取**：`BundleSecrets` 內 `OnceLock<Result<Bundle, SecretError>>`（或 `Mutex<Option<..>>` 以便 CLI 寫入後更新）；第一次 `get` 時載入，之後只讀快取。失敗也快取（見 spec），避免 30 秒輪詢反覆跳詢問。
3. **遷移在載入時做**：`get(credentials)` 回傳不存在 → 依 `secrets_cli::needed()` 的 7 個帳號名逐一讀舊項目（不存在的項目不會跳詢問）→ 有任何值則 `set(credentials, json)`。舊項目保留（使用者選定）。
4. **CLI**：`set/delete/import-env` 走 `BundleSecrets::update(|map| ...)`，一次讀、一次寫；`status` 用同一個快取載入。
5. **`register_secret`**：載入 bundle 時對每個值呼叫，維持既有日誌 redaction 行為。

## Risks / Trade-offs

- [遷移那一次仍會詢問最多 4 次] → 只發生一次；proposal 已說明。
- [bundle JSON 損毀] → 視為讀取失敗（不 fallback 到舊項目以免靜默使用過期憑證），訊息提示用 `secrets import-env` 重寫。
- [CLI 與執行中的 app 同時寫入] → app 只讀不寫（遷移除外，且遷移只在 bundle 不存在時）；CLI 寫入後 app 需重啟才讀到新值，`secrets set` 完成時印出提示。
- [單元測試無法驗證真實 macOS 詢問次數] → 以計數 `KeyStore` 替身驗證存取次數；另以實機啟動兩次（遷移、遷移後）人工記錄詢問次數，結果寫入 tasks。

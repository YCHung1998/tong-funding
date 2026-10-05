> 驗收含逐頁與 Figma 對照。UI 邏輯（連動計算、試算、確認清單、禁用判定）一律放在不依賴 GPUI 型別的 view-model 純函式，才能以 `cargo test -p app` 驗證；畫面本身以截圖對照。
> 凡改邏輯的 task，都要先寫測試並確認紅燈，再實作轉綠，並回報實際指令與測試檔路徑。

## 1. 交易單

- [x] 1.1 清單 view-model：每筆內容（含 Net Edge、取整後數量、低於最小下單量時不可勾選）、勾選／全選／全不選、選取摘要、`trigger_mode` 切換並寫入事件。測試先寫（全選、低於最小量不可選、摘要的腿數與總 Notional／Margin 手算），紅燈後實作；`cargo test -p app staged_orders`
  > 證據：`app/src/ui/vm/staged_orders_tests.rs`（`staged_orders_rows_show_floored_quantities_and_the_net_edge`、`staged_orders_select_all_skips_the_pair_below_the_minimum`、`staged_orders_summary_is_hand_calculated`、`staged_orders_mode_label_follows_the_engine`、`staged_orders_available_margin_unknown_is_never_zero`、`staged_orders_trigger_mode_toggle_sends_one_command`）；`cargo test -p tong-funding staged_orders`。紅燈：模組尚未實作時 `error[E0425]: cannot find function \`build\` in this scope`。`trigger_mode` 切換送 `SetTriggerMode`，事件 `TRIGGER_MODE_CHANGED` 由 engine 寫入（既有 engine 測試 `the_two_modes_change_independently`）。
- [x] 1.2 送出禁用判定與二次確認清單：禁用原因（未選取、設定不完整並列出缺漏欄位、kill switch、停機、非 `PREPARED`）與逐腿確認內容（含目標環境與模式）。測試窮舉每個禁用原因各一個、確認清單腿數＝2×已選配對數；確認前 mock engine 收到 0 個 Command、確認後恰好 1 個 Command，紅燈後實作
  > 證據：`each_disabled_reason_is_reported`（六個列舉各一，含 `EngineUnavailable`）、`the_confirmation_lists_two_legs_per_pair_and_nothing_is_sent_before_confirming`（4 腿 = 2×2、確認前 0 個、確認後恰好 1 個 `EnterSelected`）、`a_pair_taken_by_the_scheduler_during_confirmation_is_excluded`、`exchange_demo_confirmation_warns_about_real_demo_orders`；engine 端 `app/src/engine/actor/ui_command_tests.rs`：`auto_one_click_after_the_scheduler_took_the_pair_is_refused_and_sends_nothing_extra`、`auto_one_click_before_the_entry_time_enters_once_and_the_scheduler_does_not_enter_again`（兩種先後順序皆只有 2 張開倉單）、`one_click_is_refused_by_the_kill_switch_gate_and_sends_nothing`。紅燈：`error[E0599]: no variant named \`EnterSelected\` found for enum \`engine::command::Command\``。對話框：以內嵌確認面板實作（GPUI dialog 未驗證）。
- [x] 1.3 上次執行結果與人工處理入口：結果區塊讀自事件（重啟後仍在、標示模式、單腿失敗不得顯示「已回滾」）；`PARTIAL_FAILURE` / `IMBALANCED` / `UNRESOLVED` 配對的「人工要求平倉」「人工確認已平倉」，後者在最新持倉查詢非全平或未知時禁用。測試涵蓋兩種模式標示與兩顆按鈕的啟用條件，紅燈後實作
  > 證據：`a_simulation_result_is_labelled_and_shows_no_real_order_id`、`an_exchange_demo_result_shows_the_exchange_order_id`、`a_failed_leg_says_manual_handling_and_never_rolled_back`、`a_blocked_attempt_lists_every_failed_check`（engine 的 BLOCKED 事件新增 `failed_checks`：`a_blocked_entry_records_every_failed_check_by_name`）、`manual_handling_buttons_follow_the_latest_position_query`（非全平 / 查詢失敗 / 未成交委託 → 禁用；全平 → `ConfirmClosed`）、`manual_close_needs_its_own_confirmation_and_only_two_actions_exist`、`reconciled_pairs_offer_close_now_only_in_manual_mode`。結果來自事件（`trade_events`），重啟後由事件還原。

## 2. 合約設定

- [x] 2.1 名目、槓桿、保證金雙向連動與儲存：兩種計算模式、Decimal 計算、非法輸入的錯誤訊息、儲存後寫入 `CONTRACT_SETTINGS_UPDATED` 事件。測試先寫（1,200 ÷ 3 = 400；保證金 400 反推 3×；保證金 0 與槓桿 0 被拒），紅燈後實作
  > 證據：`app/src/ui/vm/contract_settings_tests.rs`：`leverage_mode_gives_margin_400_with_the_formula_and_pair_totals`（1,200 ÷ 3 = 400）、`margin_mode_gives_leverage_3`、`zero_margin_zero_leverage_and_zero_notional_are_refused_with_the_field_name`、`leverage_above_a_limit_only_warns`、`save_sends_one_template_command_and_an_invalid_form_sends_nothing`、`the_form_starts_from_the_python_defaults`；engine 端 `the_contract_template_is_validated_and_saved_with_its_event`（`CONTRACT_SETTINGS_UPDATED` 前後值、同一 transaction、既有配對不變）。紅燈：`cannot find type \`CalcMode\` in this scope`。
- [x] 2.2 數量試算：每所現價與新鮮度、依 `step_size` 向下取整（OKX 先換張數）、低於 `min_qty` 提示、規格或價格過期時不顯示數量。測試重用 `quantity-precision` 的數值案例（1,200 / 60,200 / 0.001 → 0.019），並加「價格過期不給數量」；`cargo test -p app contract_quote`
  > 證據：`cargo test -p tong-funding contract_quote`：`contract_quote_floors_to_the_step_1200_at_60200_is_0_019`、`contract_quote_okx_is_in_contracts_with_the_base_amount`（2 張 ≈ 0.02 BTC）、`contract_quote_below_the_minimum_shows_no_quantity`、`contract_quote_a_stale_price_gives_no_quantity`、`contract_quote_missing_rules_give_no_quantity_and_never_a_default_step`、`contract_quote_an_okx_rule_without_ct_val_is_unavailable`。紅燈：`cannot find type \`QuoteCell\` in this scope`。

## 3. 風控設定

- [x] 3.1 欄位與驗證：全域欄位、單位（秒、毫秒、百分比數值）、已移除欄位不存在、驗證訊息帶欄位名稱、儲存為單一 transaction 並寫 `RISK_CONFIG_UPDATED`（含前後值）。測試先寫（`max_leverage = 0`、`execution_mode = LIVE` 被拒且設定不變），紅燈後實作
  > 證據：`app/src/ui/vm/risk_settings_tests.rs`：`units_defaults_and_removed_fields`、`max_leverage_zero_is_refused_by_name_and_nothing_is_sent`、`the_mode_options_have_no_live_and_a_live_value_cannot_be_saved`、`saving_sends_one_command_with_both_values`；engine 端 `risk_settings_are_saved_together_with_one_event_holding_before_and_after`、`invalid_risk_settings_are_refused_by_field_and_nothing_changes`（`max_leverage = 0`、`LIVE`、覆寫 `trigger_mode` 皆拒絕且設定不變）；store 端 `config_with_event_writes_both_keys_and_the_event_or_nothing`（事件寫入失敗則兩個 key 皆回滾並停機）。紅燈：`cannot find type \`RiskForm\` in this scope`。
- [x] 3.2 Net Edge 區塊與「設定不完整」：必填欄位、`safety_margin_pct` 預設 0.01、`taker_fee_pct` 無預設；不完整狀態列出缺漏欄位並供交易單頁引用；預檢摘要列以現值呈現公式。測試：空白設定顯示不完整且列出全部缺漏欄位，填滿後轉為完整
  > 證據：`a_blank_config_is_incomplete_and_lists_every_missing_field`（五個缺漏欄位、`safety_margin_pct` 預設 0.01、費率空白不是 0）、`filling_every_required_field_makes_it_complete`、`the_formula_summary_shows_unset_instead_of_zero`。缺漏清單唯一來源 `RiskConfig::missing_fields`（`risk_settings::missing_display`），交易單頁引用同一函式。
- [x] 3.3 各交易所覆寫：九個可覆寫欄位與開關、不可覆寫欄位不提供、生效值預覽（呼叫 `effective_for_pair`）、關閉開關即移除覆寫。**整合測試**：Bybit 覆寫 `max_leverage = 4`、配對槓桿 5 → 送單前檢查 `Leverage` 對 Binance×Bybit 配對失敗、對只含未覆寫交易所的配對通過；先紅燈（覆寫未接線時應失敗）再轉綠
  > 證據：`overrides_offer_exactly_the_nine_fields`、`switching_an_override_on_starts_from_the_global_value_and_off_removes_it`（預覽 `est_slippage_pct` 取大 0.03）；**整合測試** `a_bybit_override_of_4_from_the_page_blocks_leverage_5_only_for_pairs_with_bybit`：頁面送出的 JSON 經 engine 相同的 `RiskConfig::from_json` / `parse_overrides` / `effective_for_pair` 進 `node0::run` → Binance×Bybit 只有 `Leverage` 失敗、Binance×OKX 通過。紅燈：頁面 view-model 未接線時無法編譯（`cannot find function \`evaluate\``）。
- [x] 3.4 模式單選與切換確認：只有 `SIMULATION` / `EXCHANGE_DEMO`；切到 `EXCHANGE_DEMO` 需確認，且設定不完整或 demo 金鑰不可用時禁止；切換寫入事件並即時更新狀態列徽章。測試涵蓋三個禁止／確認情境
  > 證據：`switching_to_exchange_demo_needs_a_confirmation_first`（確認前 0 個 Command；回 SIMULATION 直接送）、`exchange_demo_is_refused_while_the_config_is_incomplete`、`exchange_demo_is_refused_without_demo_keys`（含「尚未確認」）。切換事件 `EXECUTION_MODE_CHANGED` 由 engine 寫入；狀態列徽章讀 engine snapshot 的 `execution_mode`。紅燈：`cannot find function \`request_mode\``。

## 4. 手動下單

- [x] 4.1 手動下單 view-model 與命令：單腿下單（數量取整、低於最小量不送、確認清單）、撤單、`SIMULATION` 與 `EXCHANGE_DEMO` 下確認後皆恰好 1 個 Command（engine 依模式選執行器：SIMULATION → `SimulatedExecutor`，依已合併的 `engine-simulation` spec；原文「SIMULATION 下禁用」與之衝突，已更正，見 design.md 實作紀錄 #1）、確認前 0 個 Command、kill switch 下只允許 reduce-only、頁面不持有任何交易所 client（以依賴檢查證明）。測試先寫，紅燈後實作
  > 證據：`app/src/ui/vm/manual_order_tests.rs`：`only_binance_and_bybit_panels_and_a_disallowed_exchange_is_disabled`、`the_environment_text_is_truthful_in_both_modes`、`quantity_is_floored_and_shown_in_the_confirmation`（0.0014 → 0.001）、`below_the_minimum_sends_nothing`、`nothing_is_sent_before_confirming_and_exactly_one_command_after_in_exchange_demo`、`simulation_sends_the_same_single_command_to_the_engine`、`the_kill_switch_disables_only_orders_that_are_not_reduce_only`、`a_mode_change_updates_the_page_immediately`、`cancel_needs_an_order_id_and_goes_through_the_engine`、`results_are_shown_as_reported_and_a_failed_cancel_is_never_success`；依賴檢查 `ui_pages_hold_no_exchange_client`（掃描 `app/src/ui/**`，只有組裝根 `live.rs` / `wiring.rs` 與唯讀行情 `scanner_refresh.rs` 例外）；engine 端 `a_manual_cancel_goes_to_the_current_executor_and_its_answer_is_recorded_as_is`、既有 `a_manual_order_goes_through_the_simulator_with_an_intent`。紅燈：`cannot find type \`ManualForm\` in this scope`。

## 1A. 掃幣「加入交易單」與 Candidate List（spec `scanner-candidates`，先寫 spec 再實作）

- [x] 1A.1 勾選資格、Candidate List、加入並前往交易單（只送 `AddPrepared`）。
  > 證據：`app/src/ui/vm/candidates_tests.rs`（`a_qualified_tradable_fresh_row_can_be_added_and_nothing_is_sent`、`a_row_that_does_not_qualify_cannot_be_added`、`an_okx_leg_stale_data_and_an_incomplete_config_are_reasons`、`a_symbol_already_staged_cannot_be_added`、`scanner_qualification_also_requires_min_expected_net_pnl`、`candidate_views_use_the_contract_template_and_turn_invalid_when_data_changes`、`adding_sends_one_add_prepared_per_valid_candidate_and_no_order`、`an_invalid_contract_template_blocks_adding`）。紅燈：`cannot find function \`eligibility\` in this scope`。

## 0. 組裝根與 `min_expected_net_pnl_pct`

- [x] 0.1 `core`：`min_expected_net_pnl_pct`（全域、預設 0.03、≥ 0）進 `RiskConfig` / `EffectiveConfig`，Node 0 的 `NetEdgeQualified` 需兩門檻皆達（MODIFIED `risk-config` / `pretrade-validation`）。
  > 證據：`core/tests/min_expected_net_pnl.rs`（紅燈：`error[E0609]: no field \`min_expected_net_pnl_pct\` on type \`RiskConfig\``）、`engine::node0::tests::expected_net_pnl_below_min_fails_net_edge_qualified_and_equal_passes`（紅燈：assert 失敗於 node0.rs:393）。既有 core 測試未修改且全綠。
- [x] 0.2 組裝根：`main.rs` 開單一 `Db`，`LiveSource` 建真實 `EngineDeps` 並啟動 engine；engine `Snapshot` → `UiSnapshot`；頁面只經 `CommandSink` 送 `Command`。
  > 證據：`app/src/ui/vm/engine_view_tests.rs`（5 個）、`app/src/ui/wiring.rs` tests（7 個，含 `headless_subcommands_run_before_the_engine_starts`）。無顯示器、無交易所網路：實機驗證列入 TODO.md。

## 5. 驗證

- [ ] 5.1 逐頁截圖與 Figma 對照（四個畫面，逐項列出刻意差異並與 `design.md` 的差異表核對）；在 demo 帳戶走完「掃幣 → 交易單 → 持倉」一輪，並實測一次單腿失敗情境下的人工處理入口；回報實際指令、截圖路徑與紀錄

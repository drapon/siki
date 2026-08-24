# TASK-0010 検証エビデンス

**検証日**: 2026-06-23
**対象ブランチ**: feature/agent-monitoring-system

## 1. 全体ビルド・テスト（自動検証）

| 項目 | コマンド | 結果 | エビデンス |
|------|---------|------|-----------|
| ビルド | `cargo build` | ✅ PASS | exit 0（warnings 22 件はすべて既存の dead_code 等。新規エラーなし） |
| テスト | `cargo test` | ✅ PASS | `test result: ok. 469 passed; 0 failed; 0 ignored` / exit 0 |

本機能で追加した主なテスト（抜粋）:

- `db::tests::test_migration_adds_activity_column` / `test_update_session_activity`
- `hook_event::tests::test_format_activity_*`（11 件: EDGE-001 キー欠落・EDGE-002 制御文字正規化を含む）
- `session::tests::test_activity_retained_through_idle` / `test_activity_retained_through_refresh_then_idle`（REQ-103）
- `session::tests::test_working_deserialize_backward_compat_no_activity`（REQ-401 後方互換）
- `broker::tests::test_broker_persists_activity`（broker→Registry/DB 反映, REQ-101/102）
- `ui::tests::clamp_scroll_bounds`（EDGE-103/REQ-301 スクロール境界）
- `ui::tests::dashboard_rows_reflect_live_registry_update`（REQ-101 ライブ参照）
- `ui::tests::sort_orders_by_state_then_names` / `unknown_rows_not_filtered_out`（REQ-006/EDGE-003）
- `ui::left_panel::tests::resolve_project_from_worktree_row`（REQ-106）

## 2. 静的結線確認（コードによるエビデンス）

| 受け入れ条件 | 確認内容 | 結果 |
|------|---------|------|
| NFR-201 ヘルプ記載 | `src/ui/mod.rs` ヘルプに `m`/`M`/「エージェント監視ビュー (m/M)」「Esc 閉じる」を記載 | ✅ |
| REQ-003/004 トリガー | `main.rs` 左ペインで `m`→`show_agent_popup=true` / `M`→`show_agent_dashboard=true` | ✅ |
| REQ-201/105/301 横取り | `main.rs` でビュー表示中に Esc クローズ・j/k(↑↓) スクロールを早期 return で横取り | ✅ |
| render 分岐 | `render()` が registry 参照で `render_agent_popup`/`render_agent_dashboard` を呼ぶ | ✅ |
| REQ-106 解決 | `LeftPanel::resolve_project_index`（worktree 行でも所属 project に解決）+ 単体テスト | ✅ |
| バイナリ起動 | `./target/debug/siki --help` exit 0 | ✅ |

## 3. 人手による実機目視検証（要・端末操作）

以下は対話 TUI を実端末で起動して目視する必要があり、ヘッドレスのエージェント環境では実施不可。
`siki` を起動し、複数 worktree で Claude Code を動かして確認してください。

- [ ] `m`: カーソル位置プロジェクトのポップアップが開く（worktree 行でも所属プロジェクトに解決）
- [ ] `M`: 全体ダッシュボードが開く
- [ ] ツール実行に応じて activity（「Bash: …」「Edit: …」等）が 1 秒未満で反映される（NFR-001）
- [ ] 複数 worktree の行が独立にリアルタイム更新される
- [ ] alert / waiting 行が赤系で強調され、解消後に外れる（REQ-104）
- [ ] 状態優先順ソート（waiting > working > done > idle, REQ-006）が効く
- [ ] role / state / activity / 経過時間が各行に表示される（REQ-005）
- [ ] 長文 activity が `…` で省略され行崩れしない（NFR-202）
- [ ] `j`/`k` で全件スクロール、`Esc` で閉じる（REQ-301/105）
- [ ] `?`/`F1` ヘルプに `m`/`M`/`Esc` が表示される（NFR-201）
- [ ] 左ペインバッジ（●/○）・既存キーバインドに回帰がない（REQ-401）

## 結論

自動検証可能な受け入れ条件（ビルド・テスト・静的結線・後方互換）はすべて PASS。
実機目視項目は上記チェックリストに従い端末で確認すること。

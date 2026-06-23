# エージェント監視システム コンテキストノート

> 自動生成（kairo-tasknote 相当）: コードベース調査 + ユーザーヒアリングに基づく
> 対象リポジトリ: siki (TUI orchestrator for Claude Code sessions across git worktrees)

## 技術スタック

| 区分 | 内容 | 信頼性 |
|------|------|--------|
| 言語 | Rust (edition 2024) | 🔵 `Cargo.toml` |
| TUI | ratatui 0.30 + crossterm 0.29 (event-stream) | 🔵 `Cargo.toml` |
| 非同期 | tokio 1.49 (full) | 🔵 `Cargo.toml` |
| 永続化 | rusqlite 0.34 (bundled SQLite, WAL モード) | 🔵 `db.rs` |
| IPC | Unix domain socket (broker) | 🔵 `broker.rs` |
| PTY | portable-pty / tui-term / vt100 | 🔵 `Cargo.toml` |
| シリアライズ | serde / serde_json | 🔵 `Cargo.toml` |

## アーキテクチャ概要

siki は git worktree ごとに Claude Code セッションを起動し、TUI（3ペイン: 左=プロジェクト/worktree ツリー、中=Claude/ファイル、右=ソースツリー/diff）で統括する。

### セッション状態の収集経路（既存・本要件の土台）

```
Claude Code hook (PreToolUse/PostToolUse/PermissionRequest/Stop/SessionEnd)
  → `siki hook-event <state>` (hook_event.rs, stdin JSON を解釈)
  → Unix socket (~/.siki/sock)
  → Broker (broker.rs, 1接続=1行JSON)
      ├→ SQLite sessions テーブル更新 (sync_to_db)
      ├→ SessionRegistry(インメモリ) 更新 (handle_event)
      └→ TUI へ AppEvent::SessionUpdate / RefreshChanges 送信 (event_tx)
  → TUI 再描画（左ペインのバッジ ●/○ をリアルタイム更新）
```

- 状態遷移の遅延は 1 秒未満（Unix socket ローカル）。🔵 `broker.rs`
- TUI は 100ms Tick + イベント駆動で再描画。🔵 Explore 調査
- hook injection は worktree の `.claude/settings.json` に書き込み。🔵 `hooks.rs`

### 状態モデル

- `SessionState` enum: `Working` / `Waiting` / `Done` / `Idle`（`session.rs`）🔵
- `HookEvent` enum: `Register` / `Working` / `Waiting` / `Idle` / `Dead` / `Refresh`（serde tag="event", lowercase）🔵
- `VALID_HOOK_STATES = ["working","waiting","refresh","idle","dead"]`（単一真実源）🔵
- `Session` struct フィールド: `session_id, worktree_name, project_name, cwd, role, state, last_seen, idle_pending_since, alert, alert_message`（`session.rs:52`）🔵
- 集約: `aggregate_state()` が worktree 内の最優先状態を返す（Waiting>Working>Done>Idle）🔵
- スタル検知: Working/Waiting で 30 秒 Refresh が途絶えると Done 遷移、5 分無応答で削除 🔵

### DB スキーマ（sessions テーブル）

`session_id(PK), role, worktree_name, project_name, cwd, state, summary, claude_session_id, alert, alert_message, last_heartbeat, created_at`（`db.rs:21`）🔵

- マイグレーションは `ALTER TABLE ... ADD COLUMN`（失敗を握りつぶす冪等パターン）で実施済み（claude_session_id / alert / alert_message）🔵

## 本要件に関わる「現状の不足」

1. **hook がツール情報を捨てている**: `hook_event.rs` は stdin JSON から `session_id` のみ抽出。PreToolUse に含まれる `tool_name` / `tool_input` は未使用 → 「何をしているか」がツール単位で取れない。🔵 `hook_event.rs:51`
2. **全体を俯瞰する UI が無い**: 左ペインのバッジは worktree 単位の集約状態のみ。個々のエージェントの活動内容を一覧する画面が存在しない。🔵 ユーザー要望
3. **プロジェクト名クリックは開閉のみ**: `Space` で collapse/expand。詳細表示の導線が無い。🔵 README / Explore

## 既存キーバインド（トリガー設計の制約）

左ペイン: `j/k`=カーソル, `Space`=開閉, `Enter`=worktree選択, `a`=worktree追加, `A`=プロジェクト追加, `r`=run, `d`=archive/remove, `S`=siki.json。
グローバル: `q`=終了, `?`/`F1`=ヘルプ, `Tab`/`Shift+Tab`=ペイン移動。
中央: `i`=新Claudeタブ, `g`=grep, `/`=検索。

→ **`Enter`/`A`/`a`/`i`/`g` は使用済み**。監視ビュー用に未使用キーを割り当てる必要がある。🔵 README

## UI 実装パターン（再利用対象）

- ポップアップ: `ui/mod.rs` の `centered_rect(%w,%h)` → `Clear` → `Block(Borders::ALL)` → 中身。🔵 Explore
- ポップアップ状態は App の `show_*: bool` + 付随フィールドで管理。開いている間はキーを早期 return で横取り、`Esc` で閉じる。🔵 Explore
- ライブデータは `render()` が受け取る `session_registry: Option<&SessionRegistry>` 経由で参照可能。100ms Tick + SessionUpdate で自動再描画。🔵 Explore

## 開発ルール（CLAUDE.md より）

- 返答・ドキュメントは日本語。🔵
- Git の commit/push/merge/rebase/tag は `/commit` 以外で実行禁止（読み取り専用コマンドのみ可）。🔵
- 設計原則: YAGNI / 単一責任（1ファイル1目的, 200-400行目安, 最大800行）/ 段階的改善。🔵
- イミュータビリティ重視、エラーは握りつぶさず文脈付きで処理、console.log 相当を残さない。🔵
- **検証ルール**: "通るはず" で完了宣言しない。`cargo test` / `cargo build` の実行結果（exit code・件数）を確認してから主張する。🔵

## ヒアリング済み事項（確定）

| 項目 | 決定 | 信頼性 |
|------|------|--------|
| アクティビティ粒度 | レベルB（ツール単位。PreToolUse の tool_name/tool_input を整形保存） | 🔵 ヒアリング |
| トリガー方式 | 専用ホットキー（TUI で cmd/ctrl+click は不安定なため） | 🔵 ヒアリング |
| スコープ | プロジェクト別ポップアップ + 全プロジェクト横断ダッシュボードの両方 | 🔵 ヒアリング |

## 注意事項

- hook の payload は後方互換が必須。`activity` フィールドは optional（serde default）にして既存 `working` イベントと共存させる。🔵
- DB は `ALTER TABLE ADD COLUMN activity TEXT` で既存パターンに合わせて移行。🔵
- 状態系 hook の stdin タイムアウトは 1s（broker タイムアウト合計 < hook timeout 5s の制約）。activity 抽出処理を重くしない。🔵 `hook_event.rs:9-23`

# 指揮者階層アーキテクチャ タスク概要

**作成日**: 2026-07-02
**推定工数**: 62時間（約8日、1日8時間換算）
**総タスク数**: 10件
**作業規模**: 詳細タスク分割

## 関連文書

- **要件定義書**: [📋 requirements.md](../../spec/conductor-hierarchy/requirements.md)
- **ユーザストーリー**: [📖 user-stories.md](../../spec/conductor-hierarchy/user-stories.md)
- **受け入れ基準**: [✅ acceptance-criteria.md](../../spec/conductor-hierarchy/acceptance-criteria.md)
- **アーキテクチャ**: [📐 architecture.md](../../design/conductor-hierarchy/architecture.md)
- **データフロー図**: [🔄 dataflow.md](../../design/conductor-hierarchy/dataflow.md)
- **設計ヒアリング**: [💬 design-interview.md](../../design/conductor-hierarchy/design-interview.md)
- **Rust型定義**: [📝 interfaces.rs](../../design/conductor-hierarchy/interfaces.rs)
- **MCPツール仕様**: [🔌 mcp-tools.md](../../design/conductor-hierarchy/mcp-tools.md)
- **コンテキストノート**: [📝 note.md](../../spec/conductor-hierarchy/note.md)

> 本要件は Rust製TUI（siki本体）への機能追加のため、DBスキーマ変更・REST API・TypeScript型定義のタスクはなし（NFR-002によりDDL変更なし、MCPツールは`mcp-tools.md`参照）。

## フェーズ構成

| フェーズ | 目標 | タスク数 | 工数 | 対象ファイル |
|---------|------|----------|------|--------------|
| Phase 0 | dispatchプリミティブの実証 | 2 | 16h | `src/db.rs`, `src/mcp/tools.rs`, `src/mcp/protocol.rs`, `src/main.rs`, `src/app.rs`, `src/terminal.rs` |
| Phase 1 | worktree階層モデル（付け替え・ツリー描画） | 4 | 25h | `src/config.rs`, `src/app.rs`, `src/mcp/tools.rs`, `src/ui/left_panel.rs`, `src/main.rs` |
| Phase 2 | subtree一斉指示 + 状態集約 | 2 | 11h | `src/mcp/tools.rs`, `src/ui/left_panel.rs` |
| Phase 3 | 指揮者による子生成 + 運用ガイド | 2 | 10h | `src/main.rs`, `src/git.rs`, `src/mcp/tools.rs`, `README.md`/skill |

## タスク番号管理

**使用済みタスク番号**: TASK-0001 ~ TASK-0010
**次回開始番号**: TASK-0011

## 全体進捗

- [ ] Phase 0: dispatchプリミティブの実証
- [ ] Phase 1: worktree階層モデル
- [ ] Phase 2: subtree一斉指示 + 状態集約
- [ ] Phase 3: 指揮者による子生成 + 運用ガイド

## マイルストーン

- **M1: dispatch実証完了**: worktree単体への自動投入がTick経由で動作し、手動e2e検証（設計計画書Phase0検証手順）が通る
- **M2: 階層モデル完成**: 親子付け替え・循環ガード・ツリー描画・親削除時の独立化が動作する
- **M3: subtree/集約完成**: 一斉dispatch・`list_sessions(scope=children)`・状態ロールアップバッジが動作する
- **M4: 子生成+運用ガイド完成**: `spawn_child_worktree`と3ユースケースの運用手順が揃う

---

## Phase 0: dispatchプリミティブの実証

**目標**: 親→1つの子worktreeへプロンプトを自動投入して起動させる、階層の土台を実証する
**成果物**: `DispatchRow`, `get_pending_dispatches`, `dispatch` MCPツール（worktree向け）, Tick配送ロジック, リトライ+アラート

### タスク一覧

- [ ] [TASK-0001: dispatch DB層 + MCPツール基盤](TASK-0001.md) - 8h (TDD) 🔵
- [ ] [TASK-0002: TUI配送（Tick処理・リトライ・アラート）](TASK-0002.md) - 8h (TDD) 🔵

### 依存関係

```
TASK-0001 → TASK-0002
```

---

## Phase 1: worktree階層モデル

**目標**: FS2階層を維持したまま`project.json`の論理リンクで親子関係を表現し、付け替え・ツリー描画・親削除時の独立化を実現する
**成果物**: `WorktreeMeta.parent`, `get_descendants`/`would_create_cycle`, `move_worktree`, 左ペインツリー描画, 親削除フック

### タスク一覧

- [ ] [TASK-0003: 階層データモデル（WorktreeMeta.parent + config.rs関数群）](TASK-0003.md) - 7h (TDD) 🔵
- [ ] [TASK-0004: 付け替え操作（move_worktree MCPツール + TUIキー操作）](TASK-0004.md) - 6h (TDD) 🔵
- [ ] [TASK-0005: 左ペインツリー描画（親子DFS化）](TASK-0005.md) - 7h (TDD) 🔵
- [ ] [TASK-0006: 親削除時の子独立化](TASK-0006.md) - 5h (TDD) 🟡

### 依存関係

```
TASK-0002 -.実装順序(推奨).-> TASK-0003 → TASK-0004
                              TASK-0003 → TASK-0005
                              TASK-0003 → TASK-0006
```

TASK-0003 はファイル面では Phase 0 と重複せず技術的に独立だが、ユーザー指定の実装順序（Phase0→1→2→3直列）に従い Phase 0 完了後に着手する。

---

## Phase 2: subtree一斉指示 + 状態集約

**目標**: 親が配下全員に指示でき、配下の状態を吸い上げられるようにする
**成果物**: `dispatch`のsubtree対応、`list_sessions(scope=children)`、左ペインの状態ロールアップバッジ

### タスク一覧

- [ ] [TASK-0007: subtree dispatch + list_sessions(scope=children)](TASK-0007.md) - 6h (TDD) 🔵
- [ ] [TASK-0008: 左ペイン状態ロールアップ](TASK-0008.md) - 5h (TDD) 🔵

### 依存関係

```
TASK-0001, TASK-0002, TASK-0003 → TASK-0007
TASK-0003 → TASK-0008
```

TASK-0007 と TASK-0008 は互いに独立のため並行実装可能。

---

## Phase 3: 指揮者による子生成 + 運用ガイド

**目標**: 指揮者Claudeが自身配下に子worktreeを生成でき、3ユースケースの運用手順が整う
**成果物**: `create_worktree_internal`（状態レス化）、`spawn_child_worktree` MCPツール、運用ガイド文書

### タスク一覧

- [ ] [TASK-0009: 指揮者による子生成（create_worktree_internal + spawn_child_worktree）](TASK-0009.md) - 8h (TDD) 🔴
- [ ] [TASK-0010: loop運用パターンの文書化](TASK-0010.md) - 2h (DIRECT) 🔵

### 依存関係

```
TASK-0003 → TASK-0009 → TASK-0010
TASK-0002 → TASK-0010
```

---

## 信頼性レベルサマリー

### 全タスク統計

- **総タスク数**: 10件
- 🔵 **青信号**: 8件 (80%)
- 🟡 **黄信号**: 1件 (10%、TASK-0006: 親削除処理の正確なフック先が未特定)
- 🔴 **赤信号**: 1件 (10%、TASK-0009: `spawn_child_worktree`のMCP→TUIプロセス間IPC方式に前例がない)

### フェーズ別信頼性

| フェーズ | 🔵 青 | 🟡 黄 | 🔴 赤 | 合計 |
|---------|-------|-------|-------|------|
| Phase 0 | 2 | 0 | 0 | 2 |
| Phase 1 | 3 | 1 | 0 | 4 |
| Phase 2 | 2 | 0 | 0 | 2 |
| Phase 3 | 1 | 0 | 1 | 2 |

**品質評価**: 高品質（残る🟡🔴はいずれも該当タスク内で調査を伴いながら実装する方針で対応済み、design-interview.md参照）

## クリティカルパス

```
TASK-0001 → TASK-0002 → TASK-0007 → TASK-0009 → TASK-0010
```

（TASK-0003→0004/0005/0006は並行して進行可能な支流であり、TASK-0007着手までにTASK-0003が完了していればクリティカルパスに影響しない）

**クリティカルパス工数**: 8+8+6+8+2 = 32時間
**並行作業可能工数**: 30時間（TASK-0003, 0004, 0005, 0006, 0008の合計、一部並行実行可）

## 次のステップ

タスクを実装するには:
- 全タスク順番に実装: `/tsumiki:kairo-implement`
- 特定タスクを実装: `/tsumiki:kairo-implement TASK-0001`

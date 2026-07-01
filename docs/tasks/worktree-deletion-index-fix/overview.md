# worktree削除時のインデックス整合性修正 タスク概要

**作成日**: 2026-07-01
**推定工数**: 約17.5時間（1フェーズ・数日規模。1ヶ月フェーズ分割は本タスクの規模では不要）
**総タスク数**: 8件

## 関連文書

- **要件定義書**: [📋 requirements.md](../../spec/worktree-deletion-index-fix/requirements.md)
- **設計文書**: [📐 architecture.md](../../design/worktree-deletion-index-fix/architecture.md)
- **データフロー図**: [🔄 dataflow.md](../../design/worktree-deletion-index-fix/dataflow.md)
- **型定義（差分サマリ）**: [📝 interfaces.rs](../../design/worktree-deletion-index-fix/interfaces.rs)
- **コンテキストノート**: [📝 note.md](../../spec/worktree-deletion-index-fix/note.md)

## フェーズ構成

本修正はDBスキーマ・API・UIを伴わない内部ロジック修正のため、フェーズはアーキテクチャの
レイヤー（PTY系 → ClaudeSession系 → プロジェクトreindex → 統合確認）を基準に分割する。
全体で1週間未満の規模のため、1ヶ月単位の複数フェーズには分割していない。

| フェーズ | 対象レイヤー | タスク数 | 工数 | ファイル |
|---------|-------------|----------|------|----------|
| Phase 1 | PTY/Claudeタブのid解決強化（Bug A） | 2 | 5h | [TASK-0001~0002](#phase-1-ptyclaudeタブのid解決強化bug-a) |
| Phase 2 | ClaudeSessionのid化とルーティング修正（Bug B） | 3 | 6.5h | [TASK-0003~0005](#phase-2-claudesessionのid化とルーティング修正bug-b) |
| Phase 3 | プロジェクト削除時のreindex追加（Bug C） | 2 | 3.5h | [TASK-0006~0007](#phase-3-プロジェクト削除時のreindex追加bug-c) |
| Phase 4 | 統合確認・回帰防止 | 1 | 2h | [TASK-0008](#phase-4-統合確認回帰防止) |

## タスク番号管理

**使用済みタスク番号**: TASK-0001 ~ TASK-0008（本要件ディレクトリ内でのみ有効な採番）
**次回開始番号**: TASK-0009

## 全体進捗

- [ ] Phase 1: PTY/Claudeタブのid解決強化（Bug A）
- [ ] Phase 2: ClaudeSessionのid化とルーティング修正（Bug B）
- [ ] Phase 3: プロジェクト削除時のreindex追加（Bug C）
- [ ] Phase 4: 統合確認・回帰防止

## マイルストーン

- **M1: Bug A解消**: TASK-0001, TASK-0002 完了時点で、worktree削除後の別worktreeのPTY/Claude
  タブフリーズが解消
- **M2: Bug B解消**: TASK-0003〜0005 完了時点で、ClaudeSession経由の内容差し替わりが解消
- **M3: Bug C解消**: TASK-0006〜0007 完了時点で、プロジェクト削除の連鎖ズレが解消
- **M4: リリース準備完了**: TASK-0008 完了時点で `cargo build`/`cargo test` 全通過、
  EDGE-001/002 の手動確認完了

---

## Phase 1: PTY/Claudeタブのid解決強化（Bug A）

**目標**: worktree削除後、別worktreeの既に開いているPTY/Claudeタブがフリーズしないようにする
**成果物**: `resolve_claude_term_key` の修正、`resolve_terminal_key` の新設

### タスク一覧

- [ ] [TASK-0001: resolve_claude_term_keyのフォールバック範囲拡大](TASK-0001.md) - 2h (TDD) 🔵
- [ ] [TASK-0002: terminals用resolve_terminal_key新設と配線](TASK-0002.md) - 3h (TDD) 🔵

### 依存関係

```
TASK-0001 （独立）
TASK-0002 （独立、TASK-0001と並行実行可）
```

---

## Phase 2: ClaudeSessionのid化とルーティング修正（Bug B）

**目標**: ClaudeSession経由で別worktreeのチャット履歴に内容が差し替わらないようにする
**成果物**: `ClaudeSession.id`、`AppEvent` の `session_id` フィールド、`resolve_session_key`

### タスク一覧

- [ ] [TASK-0003: ClaudeSessionへのid採番追加](TASK-0003.md) - 2h (TDD) 🔵
- [ ] [TASK-0004: AppEventへのsession_idフィールド追加と送信元配線](TASK-0004.md) - 1.5h (TDD) 🔵
- [ ] [TASK-0005: resolve_session_key新設とClaudeOutput等ハンドラ書き換え](TASK-0005.md) - 3h (TDD) 🔵

### 依存関係

```
TASK-0003 → TASK-0004 → TASK-0005
```

---

## Phase 3: プロジェクト削除時のreindex追加（Bug C）

**目標**: プロジェクト削除後、後続プロジェクトのsessions/terminals/claude_termsが
正しくシフトされるようにする
**成果物**: `reindex_project_maps` とその配線

### タスク一覧

- [ ] [TASK-0006: reindex_project_maps新設](TASK-0006.md) - 2h (TDD) 🔵
- [ ] [TASK-0007: handle_remove_project_confirm_keyへの配線](TASK-0007.md) - 1.5h (TDD) 🔵

### 依存関係

```
TASK-0006 → TASK-0007
```

---

## Phase 4: 統合確認・回帰防止

**目標**: 全修正を組み合わせた状態でビルド・テストが通り、複合エッジケースが機能することを確認する
**成果物**: 全体ビルド・テスト結果、EDGE-001/002 の確認記録

### タスク一覧

- [ ] [TASK-0008: 統合ビルド・テスト確認とエッジケース検証](TASK-0008.md) - 2h (DIRECT) 🔵

### 依存関係

```
TASK-0002, TASK-0005, TASK-0007 → TASK-0008
```

---

## 信頼性レベルサマリー

### 全タスク統計

- **総タスク数**: 8件
- 🔵 **青信号**: 8件 (100%)
- 🟡 **黄信号**: 0件 (0%)
- 🔴 **赤信号**: 0件 (0%)

### フェーズ別信頼性

| フェーズ | 🔵 青 | 🟡 黄 | 🔴 赤 | 合計 |
|---------|-------|-------|-------|------|
| Phase 1 | 2 | 0 | 0 | 2 |
| Phase 2 | 3 | 0 | 0 | 3 |
| Phase 3 | 2 | 0 | 0 | 2 |
| Phase 4 | 1 | 0 | 0 | 1 |

**品質評価**: 高品質（全タスクがarchitecture.md/requirements.mdの具体的な要件ID・コード箇所に
紐づく）

## クリティカルパス

```
TASK-0003 → TASK-0004 → TASK-0005 → TASK-0008
```

**クリティカルパス工数**: 8.5時間
**並行作業可能工数**: TASK-0001(2h)・TASK-0002(3h)・TASK-0006(2h)→TASK-0007(1.5h) は
クリティカルパスと並行実行可能

## 次のステップ

タスクを実装するには:
- 全タスク順番に実装: `/tsumiki:kairo-implement`
- 特定タスクを実装: `/tsumiki:kairo-implement TASK-0001`

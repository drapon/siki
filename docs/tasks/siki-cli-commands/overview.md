# siki 操作系 CLI タスク概要

**作成日**: 2026-06-30
**推定工数**: 24時間（約3日）
**総タスク数**: 7件
**作業ブランチ**: `feature/siki-cli-commands`

## 関連文書

- **要件定義書**: [📋 requirements.md](../../spec/siki-cli-commands/requirements.md)
- **アーキテクチャ**: [📐 architecture.md](../../design/siki-cli-commands/architecture.md)
- **データフロー図**: [🔄 dataflow.md](../../design/siki-cli-commands/dataflow.md)
- **型/シグネチャ定義**: [📝 interfaces.rs](../../design/siki-cli-commands/interfaces.rs)
- **設計ヒアリング**: [💬 design-interview.md](../../design/siki-cli-commands/design-interview.md)

※ DB スキーマ / API 仕様は CLI のため非該当。

## フェーズ構成

小規模・薄い CLI ラッパのため単一フェーズ（CLI 実装）に集約。

| フェーズ | 成果物 | タスク数 | 工数 |
|---------|--------|----------|------|
| Phase 1: CLI 実装 | new/rm/path/run/list + 対話フォールバック | 7 | 24h |

## タスク番号管理

**使用済み**: TASK-0001 ~ TASK-0007
**次回開始番号**: TASK-0008

---

## Phase 1: CLI 実装

**目標**: TUI を起動せずに worktree とセッションを CLI で操作可能にする
**成果物**: `src/cli/` 一式 + `config::resolve_base_branch` 移設 + main ディスパッチ結線

### タスク一覧

- [ ] [TASK-0001: CLI モジュール土台と resolve_base_branch 移設](TASK-0001.md) - 2h (DIRECT) 🔵
- [ ] [TASK-0002: 引数スキャナ ArgScan の実装](TASK-0002.md) - 3h (TDD) 🔵
- [ ] [TASK-0003: cmd_new（worktree 作成・非対話）](TASK-0003.md) - 4h (TDD) 🔵
- [ ] [TASK-0004: cmd_rm / cmd_path / cmd_list（非対話）](TASK-0004.md) - 3h (TDD) 🔵
- [ ] [TASK-0005: cmd_run（非対話・exec 起動）](TASK-0005.md) - 4h (TDD) 🔵
- [ ] [TASK-0006: 対話セレクタ prompt.rs（crossterm）](TASK-0006.md) - 4h (TDD) 🔵
- [ ] [TASK-0007: 対話統合・ディスパッチ結線・手動E2E](TASK-0007.md) - 4h (TDD) 🔵

### 依存関係

```
TASK-0001 ─┬─ TASK-0002 ─┬─ TASK-0003 ─── TASK-0005 ─┐
           │             ├─ TASK-0004 ───────────────┤
           └─ TASK-0006 ─────────────────────────────┴─ TASK-0007
```

- TASK-0001 が全ての起点（モジュール土台・base 移設）
- TASK-0002（ArgScan）が cmd 系の前提
- TASK-0003（cmd_new）は TASK-0005（run の自動作成）の前提
- TASK-0006（prompt）は TASK-0001 後に並行可能
- TASK-0007 が全部の合流（対話接続・結線・E2E）

### 並行実行の機会

- TASK-0006（prompt.rs）は TASK-0002〜0005 と並行可能
- TASK-0004 は TASK-0003/0005 と並行可能（ArgScan 完了後）

---

## クリティカルパス

```
TASK-0001 → TASK-0002 → TASK-0003 → TASK-0005 → TASK-0007
```

**クリティカルパス工数**: 17時間
**並行可能工数**: 7時間（TASK-0004 + TASK-0006）

## 信頼性レベルサマリー

| タスク | 🔵 青 | 🟡 黄 | 🔴 赤 |
|--------|-------|-------|-------|
| TASK-0001 | 4 | 0 | 0 |
| TASK-0002 | 6 | 0 | 0 |
| TASK-0003 | 4 | 1 | 0 |
| TASK-0004 | 5 | 1 | 0 |
| TASK-0005 | 6 | 0 | 0 |
| TASK-0006 | 4 | 1 | 0 |
| TASK-0007 | 4 | 1 | 0 |
| **合計** | **33 (89%)** | **4 (11%)** | **0** |

**品質評価**: 高品質（要件・設計・ヒアリングで裏取り済み。🔴 ゼロ）

## 次のステップ

- 全タスク順番に実装: `/tsumiki:kairo-implement`
- 特定タスクを実装: `/tsumiki:kairo-implement TASK-0001`

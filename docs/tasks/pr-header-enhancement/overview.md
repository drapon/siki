# PRヘッダー強化 タスク概要

**作成日**: 2026-06-23
**推定工数**: 12時間（約1.5日）
**総タスク数**: 5件
**作業規模**: 軽量タスク分割

## 関連文書

- **要件定義書**: [📋 requirements.md](../../spec/pr-header-enhancement/requirements.md)
- **アーキテクチャ**: [📐 architecture.md](../../design/pr-header-enhancement/architecture.md)
- **データフロー図**: [🔄 dataflow.md](../../design/pr-header-enhancement/dataflow.md)
- **設計ヒアリング**: [💬 design-interview.md](../../design/pr-header-enhancement/design-interview.md)

> 本要件は Rust TUI への機能追加のため、DB スキーマ / API 仕様 / TS 型定義のタスクはなし。

## フェーズ構成

| フェーズ | 成果物 | タスク数 | 工数 | 対象ファイル |
|---------|--------|----------|------|--------------|
| Phase 1 | データ基盤 | 1 | 2h | `src/app.rs` |
| Phase 2 | 取得・イベント層 | 2 | 5h | `src/main.rs`, `src/event.rs` |
| Phase 3 | 表示・操作層 | 2 | 5h | `src/ui/main_panel.rs`, `src/main.rs` |

## タスク番号管理

**使用済みタスク番号**: TASK-0001 ~ TASK-0005
**次回開始番号**: TASK-0006

## 全体進捗

- [ ] Phase 1: データ基盤
- [ ] Phase 2: 取得・イベント層
- [ ] Phase 3: 表示・操作層

---

## Phase 1: データ基盤

**目標**: PR 表示情報の型と状態フィールドを用意する
**成果物**: `PrStatus` / `PrInfo` 型、`Worktree.pr`、`App.pr_link_area`

### タスク一覧

- [ ] [TASK-0001: PR データモデル追加](TASK-0001.md) - 2h (TDD) 🔵

---

## Phase 2: 取得・イベント層

**目標**: gh から PR 情報を取得し状態判定して App 状態へ反映する
**成果物**: `classify_pr_status` 純粋関数、`fetch_pr_info`、`AppEvent::PrInfo` 配線

### タスク一覧

- [ ] [TASK-0002: 状態判定の純粋関数 + PR 情報取得の拡張](TASK-0002.md) - 3h (TDD) 🔵
- [ ] [TASK-0003: AppEvent::PrInfo の配線変更](TASK-0003.md) - 2h (TDD) 🔵

### 依存関係

```
TASK-0001 → TASK-0002 → TASK-0003
```

---

## Phase 3: 表示・操作層

**目標**: ヘッダーに番号・状態色を描画し、クリックでブラウザを開く
**成果物**: 更新された `render_branch_header`、クリック判定、`open_in_browser`

### タスク一覧

- [ ] [TASK-0004: ヘッダー描画の更新（タイトル #123 + 状態色 + 領域記録）](TASK-0004.md) - 3h (TDD) 🔵
- [ ] [TASK-0005: PR 部分クリックでブラウザ起動](TASK-0005.md) - 2h (TDD) 🔵

### 依存関係

```
TASK-0003 → TASK-0004 → TASK-0005
```

---

## 信頼性レベルサマリー

### 全タスク統計

- **総タスク数**: 5件
- 🔵 **青信号**: 5件 (100%) — 全タスクが要件・設計から確実に導出
- 🟡 **黄信号**: 0件（タスク単位。項目単位では多OSブラウザ起動・非フォーカス時グレー化の2項目が🟡）
- 🔴 **赤信号**: 0件

### フェーズ別

| フェーズ | 🔵 | 🟡 | 🔴 | 合計 |
|---------|----|----|----|------|
| Phase 1 | 1 | 0 | 0 | 1 |
| Phase 2 | 2 | 0 | 0 | 2 |
| Phase 3 | 2 | 0 | 0 | 2 |

**品質評価**: 高品質

## クリティカルパス

```
TASK-0001 → TASK-0002 → TASK-0003 → TASK-0004 → TASK-0005
```

**クリティカルパス工数**: 12時間（全タスクが直列依存。並行実行可能なタスクなし）

## 次のステップ

- 全タスク順番に実装: `/tsumiki:kairo-implement`
- 特定タスクを実装: `/tsumiki:kairo-implement TASK-0001`

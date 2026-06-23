# エージェント監視システム タスク概要

**作成日**: 2026-06-23
**プロジェクト期間**: Day 1 - Day 4（実働 約4日 / 26時間）
**推定工数**: 26時間
**総タスク数**: 10件
**作業規模**: 詳細タスク分割（コンポーネント単位）

## 関連文書

- **要件定義書**: [📋 requirements.md](../../spec/agent-monitoring-system/requirements.md)
- **受け入れ基準**: [✅ acceptance-criteria.md](../../spec/agent-monitoring-system/acceptance-criteria.md)
- **設計文書**: [📐 architecture.md](../../design/agent-monitoring-system/architecture.md)
- **データフロー図**: [🔄 dataflow.md](../../design/agent-monitoring-system/dataflow.md)
- **型定義（Rust）**: [📝 interfaces.rs](../../design/agent-monitoring-system/interfaces.rs)
- **データベース設計（SQLite）**: [🗄️ database-schema.sql](../../design/agent-monitoring-system/database-schema.sql)
- **コンテキストノート**: [📝 note.md](../../spec/agent-monitoring-system/note.md)

> **API仕様（api-endpoints.md）は無し**: 本機能は HTTP API を持たない TUI 機能。

## フェーズ構成

| フェーズ | 期間 | 成果物 | タスク数 | 工数 | ファイル |
|---------|------|--------|----------|------|----------|
| Phase 1 | Day 1-2 | hook 拡張・データ層（activity 取得/保持/永続） | 4 | 10h | [TASK-0001~0004](#phase-1-hook拡張データ層) |
| Phase 2 | Day 2-4 | UI（プロジェクト別ポップアップ・全体ダッシュボード・キー操作） | 4 | 11h | [TASK-0005~0008](#phase-2-ui実装) |
| Phase 3 | Day 4 | リアルタイム確認・エッジ仕上げ・実機検証 | 2 | 5h | [TASK-0009~0010](#phase-3-リアルタイム確認仕上げ検証) |

## タスク番号管理

**使用済みタスク番号**: TASK-0001 ~ TASK-0010
**次回開始番号**: TASK-0011

## 全体進捗

- [ ] Phase 1: hook 拡張・データ層
- [ ] Phase 2: UI 実装
- [ ] Phase 3: リアルタイム確認・仕上げ・検証

## マイルストーン

- **M1: データ層完成** (Day 2): activity が hook→broker→Registry/DB に流れる
- **M2: UI 完成** (Day 4): m/M で2ビューが開き activity がリアルタイム表示
- **M3: リリース準備完了** (Day 4): 全 cargo test グリーン・実機検証完了

---

## Phase 1: hook拡張・データ層

**期間**: Day 1-2
**目標**: PreToolUse からツール activity を取得し、Registry と SQLite に保持・永続する
**成果物**: activity を運ぶ hook payload / 拡張された状態モデル / DB マイグレーション

### タスク一覧

- [ ] [TASK-0001: DB層 activity 列追加と update_session_activity](TASK-0001.md) - 2h (TDD) 🔵
- [ ] [TASK-0002: 状態モデル HookEvent.activity / Session.activity と Registry 反映](TASK-0002.md) - 3h (TDD) 🔵
- [ ] [TASK-0003: hook 抽出 format_activity と working 拡張](TASK-0003.md) - 3h (TDD) 🔵
- [ ] [TASK-0004: broker 連携 activity の DB 永続と Registry 反映](TASK-0004.md) - 2h (TDD) 🔵

### 依存関係

```
TASK-0001 ┐
TASK-0002 ┼→ TASK-0004
TASK-0002 → TASK-0003 → TASK-0004
```

（TASK-0001 と TASK-0002 は前提なしで並行着手可）

---

## Phase 2: UI実装

**期間**: Day 2-4
**目標**: 監視2ビューを実装し、専用ホットキーで開閉・スクロールできるようにする
**成果物**: render_agent_popup / render_agent_dashboard / app 状態 / m・M・Esc・j/k キー操作

### タスク一覧

- [ ] [TASK-0005: app 状態 ポップアップ状態フィールド追加](TASK-0005.md) - 1h (DIRECT) 🔵
- [ ] [TASK-0006: プロジェクト別ポップアップ描画 render_agent_popup](TASK-0006.md) - 4h (TDD) 🔵
- [ ] [TASK-0007: 全体ダッシュボード描画 render_agent_dashboard と状態優先ソート](TASK-0007.md) - 3h (TDD) 🔵
- [ ] [TASK-0008: キー操作 m/M・Esc・j/k・help・render 分岐](TASK-0008.md) - 3h (TDD) 🔵

### 依存関係

```
TASK-0002 ┐
TASK-0005 ┼→ TASK-0006 ┐
TASK-0005 ┴→ TASK-0007 ┼→ TASK-0008
```

（TASK-0005 は前提なし。TASK-0006/0007 は TASK-0002・0005 完了後に並行着手可）

---

## Phase 3: リアルタイム確認・仕上げ・検証

**期間**: Day 4
**目標**: リアルタイム更新とエッジ整形を確認し、実機で全要件を検証する
**成果物**: エッジ網羅テスト / 実機検証エビデンス / 全 cargo test グリーン

### タスク一覧

- [ ] [TASK-0009: リアルタイム更新・エッジ整形の検証とテスト](TASK-0009.md) - 3h (TDD) 🔵
- [ ] [TASK-0010: 実機検証 複数 worktree での反映と全体ビルド/テスト](TASK-0010.md) - 2h (DIRECT) 🔵

### 依存関係

```
TASK-0004 ┐
TASK-0008 ┴→ TASK-0009 → TASK-0010
```

---

## 信頼性レベルサマリー

### 全タスク統計（タスク全体評価ベース）

- **総タスク数**: 10件
- 🔵 **青信号**: 10件 (100%)
- 🟡 **黄信号**: 0件 (0%)
- 🔴 **赤信号**: 0件 (0%)

> 注: タスク内の個別項目レベルでは、activity 整形の細部（Task/Grep フォーマット）や正規化幅などに 🟡 が一部含まれる（主に TASK-0003/0006）。タスク全体の方針はすべて要件・設計・ヒアリングに接地しており 🔵。

### フェーズ別信頼性（タスク数）

| フェーズ | 🔵 青 | 🟡 黄 | 🔴 赤 | 合計 |
|---------|-------|-------|-------|------|
| Phase 1 | 4 | 0 | 0 | 4 |
| Phase 2 | 4 | 0 | 0 | 4 |
| Phase 3 | 2 | 0 | 0 | 2 |

**品質評価**: 高品質（粒度=コンポーネント単位で適切、依存関係明確、実装可能性は既存コードに接地）

## クリティカルパス

```
TASK-0002 → TASK-0006 → TASK-0008 → TASK-0009 → TASK-0010
（並行: TASK-0001 → TASK-0004 → TASK-0009、TASK-0007 → TASK-0008）
```

**クリティカルパス工数**: 約15時間（0002:3 + 0006:4 + 0008:3 + 0009:3 + 0010:2）
**並行作業可能工数**: 約11時間（0001/0003/0004/0005/0007）

## タスクタイプ内訳

- **TDD**: 8件（0001/0002/0003/0004/0006/0007/0008/0009）
- **DIRECT**: 2件（0005 フィールド追加 / 0010 実機検証）

## 次のステップ

タスクを実装するには:
- 全タスク順番に実装: `/tsumiki:kairo-implement`
- 特定タスクを実装: `/tsumiki:kairo-implement TASK-0001`
- 依存順の推奨着手: TASK-0001 / TASK-0002 / TASK-0005（前提なし）から

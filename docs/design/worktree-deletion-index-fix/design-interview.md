# worktree削除時のインデックス整合性修正 設計ヒアリング記録

**作成日**: 2026-07-01
**ヒアリング実施**: step4 既存情報ベースの差分ヒアリング（フル設計）

## ヒアリング目的

要件定義フェーズ（`docs/spec/worktree-deletion-index-fix/requirements.md`）で確定した
「グローバル一意idによる実体解決」方針を、実装可能な設計へ落とし込むため、実装の分割単位・
id採番方法・reindex関数の構造・テスト範囲について確認した。

## 質問と回答

### Q1: id解決ロジックの共通化方針

**質問日時**: 2026-07-01
**カテゴリ**: アーキテクチャ
**背景**: `terminals`/`claude_terms`/`sessions` はいずれも id ベースの再探索が必要になるが、
共通トレイトで一本化するか、既存の `resolve_claude_term_key` と同型の専用関数を3つ書くかで
実装の複雑さが変わるため確認した。

**回答**: マップごとに個別関数（推奨）

**信頼性への影響**:
- architecture.md の「解決関数は用途ごとに個別実装」を 🔵 で確定。`resolve_terminal_key`/
  `resolve_session_key` を専用関数として設計した

---

### Q2: ClaudeSession の id 採番カウンタ共有可否

**質問日時**: 2026-07-01
**カテゴリ**: 技術選択
**背景**: `TerminalEmulator` には既に `NEXT_TERMINAL_ID`（グローバル `AtomicU64`）があり、
これを `ClaudeSession` にも流用するか、独立させるかで実装箇所とモジュール結合度が変わる。

**回答**: claude.rsに別カウンタ（推奨）

**信頼性への影響**:
- `NEXT_SESSION_ID: AtomicU64`（`claude.rs` 内）を新設する設計に 🔵 で確定。
  `TerminalEmulator` と `ClaudeSession` のモジュール結合を増やさない

---

### Q3: reindex_project_maps の実装形態

**質問日時**: 2026-07-01
**カテゴリ**: アーキテクチャ
**背景**: プロジェクト削除用のreindexを、worktree削除用の既存 `reindex_worktree_maps` と
共通化（次元をパラメータ化）するか、別関数として新設するかで設計の見通しが変わる。

**回答**: 別関数として新設（推奨）

**信頼性への影響**:
- `reindex_project_maps` を新設関数として設計 🔵 に確定。既存 `reindex_worktree_maps` は
  シグネチャ・実装ともに変更しない

---

### Q4: テスト範囲（ユニットテスト vs 統合テスト）

**質問日時**: 2026-07-01
**カテゴリ**: パフォーマンス・スケーラビリティ（テスト実行速度の観点も含む）
**背景**: 実際にPTY/プロセスをspawnする統合テストは実行が遅く不安定になりやすいため、
解決関数・reindex関数を対象にした純粋なユニットテストで十分かを確認した。

**回答**: 純粋ユニットテストのみ（推奨）

**信頼性への影響**:
- テスト方針を「HashMapとidを手動で構築し、解決関数・reindex関数のみを検証する」ことに
  🔵 で確定（既存 `test_remove_worktree` 等と同様のパターン）

---

## ヒアリング結果サマリー

### 確認できた事項
- id解決は用途別に3つの専用関数として実装する（過度な抽象化を避ける）
- `ClaudeSession` の id 採番は独立したカウンタとする
- プロジェクト単位のreindexは独立した新設関数とする
- テストは純粋ユニットテストの範囲に収める（統合テストは対象外）

### 設計方針の決定事項
- `resolve_claude_term_key`（既存）はフォールバックの絞り込み条件のみ修正し、シグネチャは
  変えない
- `resolve_terminal_key`／`resolve_session_key`（新設）は同型の「直接キー引き→id全体走査」
  パターンを踏襲する
- `reindex_project_maps`（新設）は「削除位置より後ろのキーを収集→remove→新キーでinsert」
  という既存 `reindex_worktree_maps` と同じ手続きパターンを踏襲する

### 残課題
- 実装時にコンパイラの借用チェックにより、`HashMap` の走査と `remove`/`insert` の順序
  調整が必要になる可能性がある（既存 `reindex_worktree_maps` の実装パターンをそのまま
  踏襲すれば回避できる見込み）

### 信頼性レベル分布

**ヒアリング前**（要件定義書ベースの設計素案時点）:
- 🔵 青信号: 約10件
- 🟡 黄信号: 約3件
- 🔴 赤信号: 0件

**ヒアリング後**:
- 🔵 青信号: 30件 (+20)
- 🟡 黄信号: 3件 (±0)
- 🔴 赤信号: 0件 (±0)

## 関連文書

- **アーキテクチャ設計**: [architecture.md](architecture.md)
- **データフロー**: [dataflow.md](dataflow.md)
- **型定義（差分サマリ）**: [interfaces.rs](interfaces.rs)
- **要件定義**: [requirements.md](../../spec/worktree-deletion-index-fix/requirements.md)

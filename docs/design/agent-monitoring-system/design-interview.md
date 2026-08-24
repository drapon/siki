# エージェント監視システム 設計ヒアリング記録

**作成日**: 2026-06-23
**ヒアリング実施**: step4 既存情報ベースの差分ヒアリング（設計フェーズ）

## ヒアリング目的

要件定義（requirements.md）で確定済みの方針を前提に、技術設計を確定するための残課題（activity 整形ルール・unknown セッションの扱い・スクロール・レイアウト）を明確化した。

## 質問と回答

### Q1: 設計の作業規模

**質問日時**: 2026-06-23
**カテゴリ**: アーキテクチャ
**背景**: 設計文書の詳細度（フル/軽量/カスタム）を決めるため。

**回答**: フル設計（architecture / dataflow / design-interview に加え、Rust 型定義・SQLite スキーマを作成）。

**信頼性への影響**: 成果物範囲が確定。

---

### Q2: 既存実装の追加詳細分析の要否

**質問日時**: 2026-06-23
**カテゴリ**: アーキテクチャ
**背景**: 要件定義フェーズで hook_event/session/broker/db と UI パターンを調査済み。行単位の再調査が必要か。

**回答**: 不要。調査済みの理解で設計する。

**信頼性への影響**: 直接の変化なし（既存調査で十分と判断）。

---

### Q3: Bash ツールの activity 整形（何を優先するか）

**質問日時**: 2026-06-23
**カテゴリ**: データモデル（表示文字列）
**背景**: PreToolUse の Bash tool_input には `command` と `description` の両方があり得る。どちらを activity に出すかで読みやすさ/正確さが変わる。

**回答**: description 優先（無ければ command にフォールバック）。

**信頼性への影響**: REQ-002 / `format_activity` の Bash 分岐を 🔵 に確定（interfaces.rs に反映）。

---

### Q4: unknown/unknown セッションのダッシュボード表示（EDGE-003）

**質問日時**: 2026-06-23
**カテゴリ**: データモデル
**背景**: working が未登録セッションで来ると cwd 空・project/worktree="unknown" で自動登録される（`session.rs:354`）。これをダッシュボードに出すか抑制するか。

**回答**: 表示する（取りこぼし防止・異常検知）。

**信頼性への影響**: EDGE-003 を 🔵 に確定。ダッシュボードは `all()` を無フィルタで取得する設計に確定（dataflow.md 機能3）。

---

### Q5: 項目数超過時のスクロール（REQ-301 / EDGE-103）

**質問日時**: 2026-06-23
**カテゴリ**: アーキテクチャ（UI）
**背景**: セッションが多いと表示領域に収まらない。スクロール対応するか Phase1 はビューポート内のみとするか。

**回答**: `j`/`k` スクロール対応（help ポップアップと同様）。

**信頼性への影響**: REQ-301 を 🔵 に確定。app.rs にスクロール状態（`agent_popup_scroll` / `agent_dashboard_scroll`）を持たせる設計に確定。

---

### Q6: 2ビューのレイアウト

**質問日時**: 2026-06-23
**カテゴリ**: アーキテクチャ（UI）
**背景**: ポップアップとダッシュボードのサイズ・配置をどうするか。

**回答**: 両方中央ポップアップ（centered_rect）。ポップアップ=60×50%、ダッシュボード=80×80%。

**信頼性への影響**: 描画関数のサイズ指定を 🔵 に確定（architecture.md UI 層・interfaces.rs 描画シグネチャ）。

---

## ヒアリング結果サマリー

### 確認できた事項
- 既存アーキテクチャ（hook→broker→Registry/DB→TUI）を維持し、activity を1つ載せる加算的拡張で要件を満たせる。
- UI はインメモリ Registry をライブ参照し、AppEvent::SessionUpdate の payload 拡張は不要。

### 設計方針の決定事項
- activity 整形: Bash=description優先、ファイル系=basename、Task=subagent_type+description、他=tool_name（EDGE-001/002 で正規化・省略）。
- DB 変更は sessions への `activity TEXT` 1列のみ（冪等 ALTER）。
- ダッシュボードは unknown も含め全件を状態優先順ソート、`j`/`k` スクロール。
- レイアウトは中央ポップアップ（60×50 / 80×80）。

### 残課題（実装フェーズで確定する細部）
- `format_activity` の Task/Grep/Glob/WebFetch 等の細かなフォーマット（🟡）。
- 正規化（改行・制御文字）と省略幅の具体値（🟡）。
- `m`/`M` のキー登録箇所（`m` は左ペイン文脈、`M` をグローバルにするか左ペイン限定にするか）— 実装時に main.rs のディスパッチ構造に合わせて確定。

### 信頼性レベル分布

**ヒアリング前**（要件確定済み・設計未着手）:
- 🔵 青信号: 多数（要件レベルは確定）
- 🟡 黄信号: 整形/表示/レイアウトの設計細部が未確定
- 🔴 赤信号: 0

**ヒアリング後**（設計文書全体）:
- 🔵 青信号: 64
- 🟡 黄信号: 9
- 🔴 赤信号: 0

## 関連文書

- **アーキテクチャ設計**: [architecture.md](architecture.md)
- **データフロー**: [dataflow.md](dataflow.md)
- **型定義（Rust）**: [interfaces.rs](interfaces.rs)
- **DBスキーマ（SQLite）**: [database-schema.sql](database-schema.sql)
- **要件定義**: [requirements.md](../../spec/agent-monitoring-system/requirements.md)

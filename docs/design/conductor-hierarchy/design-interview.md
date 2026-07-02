# 指揮者階層アーキテクチャ 設計ヒアリング記録

**作成日**: 2026-07-02
**ヒアリング実施**: step4 既存情報ベースの差分ヒアリング + コードベース実地調査（Exploreエージェント）

## ヒアリング目的

要件定義書（`docs/spec/conductor-hierarchy/requirements.md`）を実装可能な技術設計へ落とし込むにあたり、(1) 未確定だった実装パラメータの確認、(2) 実際のソースコード（`src/db.rs`, `src/mcp/tools.rs`, `src/mcp/mod.rs`, `src/mcp/protocol.rs`, `src/main.rs`, `src/terminal.rs`, `src/config.rs`, `src/app.rs`, `src/ui/left_panel.rs`, `src/session.rs`, `src/hooks.rs`, `src/git.rs`）を実地調査し、設計計画書・コンテキストノートの記述が現状のコードと一致するかを検証した。

## 質問と回答

### Q1: dispatchリトライ上限の具体的な閾値

**質問日時**: 2026-07-02
**カテゴリ**: 未定義設計の詳細化
**背景**: requirements.md REQ-006 は「目安30回≒3秒」として閾値を未確定のままにしていた。

**回答**: 30回（約3秒）を採用。

**信頼性への影響**: architecture.md の `DISPATCH_RETRY_LIMIT: u32 = 30` として確定（🔵）。

---

### Q2: dispatchリトライ状態の保持場所

**質問日時**: 2026-07-02
**カテゴリ**: データモデル
**背景**: `messages` テーブルはDDL変更しない方針（NFR-002）のため、「何回リトライしたか」というカウンタをどこに保持するかが未確定だった。

**回答**: TUIプロセスのインメモリ（`HashMap<i64, u32>`、dispatch_id→リトライ回数）に保持する。

**信頼性への影響**: `claude_terms` と同じ寿命・スコープ（TUIプロセスの `main()` イベントループ内ローカル変数）で `dispatch_retry_counts: HashMap<i64, u32>` を持つ設計に確定（🔵）。TUI再起動時にリセットされるが、再起動後は次回Tickから既読未処理のdispatchレコードに対しリトライが0からやり直しになるだけで、機能的な問題はない。

---

### Q3: 親子付け替えMCPツールの命名

**質問日時**: 2026-07-02
**カテゴリ**: 技術選択（API命名）
**背景**: 設計計画書は `move_worktree` と `set_parent` の両方を候補として挙げていた。

**回答**: `move_worktree` を採用。

**信頼性への影響**: MCPツール仕様（`mcp-tools.md`）で `move_worktree` として確定（🔵）。

---

## コードベース実地調査で判明した重要な訂正事項

Exploreエージェントによる実地調査（`src/db.rs`, `src/mcp/tools.rs`, `src/mcp/mod.rs`, `src/mcp/protocol.rs`, `src/main.rs`, `src/terminal.rs`, `src/config.rs`, `src/app.rs`, `src/ui/left_panel.rs`, `src/session.rs`, `src/hooks.rs`, `src/git.rs` を実際に読んで検証）の結果、設計計画書・要件定義書の一部記述が現在のコードと異なることが判明した。

### 訂正1: PreToolUse hookは危険ツールを「ゲート」しない（重要・安全根拠に影響）

**旧記述（要件定義書 REQ-008, NFR-101）**: 「ワーカー側の危険ツール実行は既存のPreToolUse hookによる承認フローで引き続きゲートされる」

**実際のコード（`src/hooks.rs:36-47`）**: siki の `PreToolUse` hook（`{siki_path} hook-event working`）は `is_async: true` で注入されており、**セッション状態を`working`にする通知専用**。ツール名を見て許可/拒否を判断するロジックは一切ない（fire-and-forget）。

**訂正内容**: 危険ツールの実際の安全弁は siki の hook ではなく、**Claude Code CLI 自体が持つ組み込みの権限承認システム**（siki hookとは独立に常時動作するツール実行前の承認プロンプト）である。完全自動投入という結論（REQ-008）は変わらないが、安全根拠の記述を要件定義書側も訂正済み（🔴→🔵）。

### 訂正2: `resolve_worktree_id` 相当の関数は存在しない

**旧記述（設計計画書 Phase0-3）**: 「`resolve_worktree_id`は`app.projects`走査の小ヘルパ」（既存すると読める書き方）

**実際のコード**: `worktree_name（+project_name）→ WorktreeId` の逆引き関数は `main.rs`/`app.rs` のどこにも存在しない。既存コードは全て「既にindexを持っている」箇所か、逆方向（index→name）の変換のみ。

**訂正内容**: 本設計で `App::find_worktree_id(&self, project_name: &str, worktree_name: &str) -> Option<WorktreeId>` を**新規作成**する（`architecture.md` 参照）。

### 訂正3: MCPツールのJSONスキーマは `mcp/mod.rs` ではなく `mcp/protocol.rs` にある

**旧記述**: 「`mod.rs` のツール一覧/schema/instructions」

**実際のコード**: instructions文字列は `mcp/mod.rs:72` にあるが、`ToolDefinition` 構造体と `tool_definitions()`（全ツールのJSON Schema一覧）は `mcp/protocol.rs:53-230` にある。新規ツール（`dispatch`, `move_worktree`, `spawn_child_worktree`）のスキーマ追加先は `protocol.rs` である。

### 訂正4: `target:{type,id}` の3方向振り分けは3箇所に重複している

**実際のコード**: `send_message`（`tools.rs:164-169`）、`handoff`（`tools.rs:280-285`）、`get_context`（`tools.rs:321-326`）の3箇所に同一の `match target_type { "session"=>.., "worktree"=>.., "project"=>.. }` が重複実装されている。`dispatch` に `"subtree"` を追加する場合、この3箇所とは別に新規関数として実装するため直接の影響はないが、既存の三重重複はコードベースの既知の構造的特徴として記録する。

### 訂正5: `TerminalEmulator::write` はPTYが死んでいても `Ok(())` を返す

**実際のコード（`src/terminal.rs:231-247`）**: `!self.alive` の場合、何も書き込まず即座に `Ok(())` を返す（サイレントno-op）。`alive` フィールドはprivate。

**設計への影響**: REQ-003「書き込み成功時にのみ既読化」の「成功」判定に `write()` の戻り値だけを使うと、PTYが死んでいても既読化されてしまう。本設計では `TerminalEmulator` に `pub fn is_alive(&self) -> bool` アクセサを新規追加し、`is_alive() && write(...).is_ok()` を成功条件とする（`architecture.md` 参照）。

### 訂正6: `SessionRegistry` は `session_id` でキーされ、`(project_name, worktree_name)` では引けない

**旧記述**: 「`SessionRegistry`は`(project_name,worktree_name)`文字列でしか識別しない」

**実際のコード（`session.rs:117-120`）**: `SessionRegistry { sessions: HashMap<String, Session> }` は `session_id` がキー。`by_worktree`/`aggregate_state`/`has_alert` は `(project_name, worktree_name)` フィールドで全件線形走査（O(n)）するメソッドとして提供されている。Phase2の状態ロールアップはこれらの既存メソッドを子孫名の集合に対して繰り返し呼ぶ形で実現できる（新規インデックス構造は不要）。

### 訂正7: `broadcast` ツールは既存で `scope` パラメータを無視している（既知のバグ・本要件のスコープ外）

調査中に発見した既存の不整合。`protocol.rs` のスキーマは `scope:"machine"|"project"` を宣言しているが、`tools.rs::broadcast` の実装は `params.get("scope")` を一切読まず常に全体ブロードキャストする。本要件（指揮者階層）とは無関係の既存バグのため、本設計では修正対象に含めない。

## ヒアリング結果サマリー

### 確認できた事項
- リトライ上限=30回、保持場所=TUIプロセスのインメモリHashMap
- MCPツール名は `move_worktree` に確定

### 設計方針の決定事項
- `resolve_worktree_id`相当の新規関数 `App::find_worktree_id` を新規実装する
- `TerminalEmulator` に `is_alive()` アクセサを新規追加する
- 新規MCPツール（`dispatch`, `move_worktree`, `spawn_child_worktree`）のスキーマは `protocol.rs` に追加する
- REQ-008/NFR-101の安全根拠の記述を requirements.md 側も訂正済み

### 残課題（`/tsumiki:kairo-tasks` のタスク分割時に解消済み）
- ~~親worktree削除時（REQ-016）にフックすべき既存の「worktree削除処理」の正確な関数名・行番号は本ラウンドの調査で特定できなかった~~ → **解決済み**: タスク分割時の実地調査で、実際の削除処理は `main.rs` の `handle_archive_confirm_key`（'y'確定分岐）内であり、`reindex_worktree_maps`はメモリ上のインデックス再構築のみでproject.json書き込みとは無関係と判明（TASK-0006.md参照）🔵
- ~~`save_worktree_display_name`の正確なシグネチャ・実装は本ラウンドでは未取得~~ → **解決済み**: `src/config.rs:355-383`で確認。**重要な追加発見**: この関数は`display_name=None`指定時にWorktreeMeta**エントリ全体を削除**する挙動を持つため、`save_worktree_parent`はこの削除的挙動を踏襲してはならない（`parent`のみをクリアしエントリ自体は残す必要がある）。TASK-0003.mdに明記済み 🔵
- **新規発見**（設計フェーズには無かった訂正）: `TerminalEmulator::is_alive()`は設計時点（architecture.md訂正5）では「新規追加が必要」としていたが、タスク分割時の実地調査で`src/terminal.rs:254-257`に**既に実装済み**であることが判明。TASK-0002.mdでは「新規追加」ではなく「既存確認のみ」に修正済み 🔵

### 信頼性レベル分布

**ヒアリング前**（requirements.md確定時点）:
- 🔵 青信号: 24件
- 🟡 黄信号: 7件
- 🔴 赤信号: 1件

**ヒアリング後**（コード実地調査・design hearing完了後）:
- 🔵 青信号: 32件 (+8、コード調査による裏付けと訂正の確定)
- 🟡 黄信号: 6件 (-1、一部が🔵へ、一部残課題として残存)
- 🔴 赤信号: 1件 (変わらず、EDGE-002は本設計でも未規定)

## 関連文書

- **アーキテクチャ設計**: [architecture.md](architecture.md)
- **データフロー**: [dataflow.md](dataflow.md)
- **Rust型定義**: [interfaces.rs](interfaces.rs)
- **MCPツール仕様**: [mcp-tools.md](mcp-tools.md)
- **要件定義**: [requirements.md](../../spec/conductor-hierarchy/requirements.md)

# 指揮者階層アーキテクチャ 要件定義書

**作成日**: 2026-07-02
**作業規模**: フル機能開発（Phase 0〜3を一括で要件化）
**対象**: siki 本体（`src/db.rs`, `src/mcp/tools.rs`, `src/mcp/mod.rs`, `src/main.rs`, `src/config.rs`, `src/app.rs`, `src/ui/left_panel.rs`, `src/session.rs` 他）

## 概要

現状の siki は「人間が TUI で複数 Claude Code セッションを束ね、セッション同士が MCP メッセージを送り合う peer-to-peer」構成であり、指揮者（オーケストレーター）の役割は人間が担っている。本要件は、**1つの指揮者worktreeのAIが配下worktreeへ指示を出し、配下が自動で動き出し、状況を吸い上げる**仕組みを siki に追加する。

想定ユースケース（すべて worktree 横断・使い分けはユーザーに委ねる）:

1. **実装指揮**: 実装中の指揮者worktreeのClaudeが各子worktreeの実装状況を把握し、実装自体は子worktree内で走る。
2. **レビュー応答指揮**: 指揮者worktree側で `/loop` を回し、PRに来たコメントへの返答・修正を子へ指示する。
3. **レビュー指揮**: 親worktreeでレビューloopを回しつつ、レビュー対象を子worktreeとして取得し、親が子へ指示、子それぞれがレビューする。

「指揮者」は独立エンティティではなく、**子worktreeを持つworktreeが指揮者**（創発的）。指揮者worktreeはいくらでも作れ、子worktreeは指揮者間で付け替え可能（A指揮者→B指揮者）。

## 関連文書

- **ヒアリング記録**: [💬 interview-record.md](interview-record.md)
- **ユーザストーリー**: [📖 user-stories.md](user-stories.md)
- **受け入れ基準**: [✅ acceptance-criteria.md](acceptance-criteria.md)
- **コンテキストノート**: [📝 note.md](note.md)
- **設計計画書**: `~/.claude/plans/quizzical-frolicking-gizmo.md`（本会話外・ユーザーローカル、Phase 0〜3の実装詳細を含む一次資料）

## 技術的前提（確定制約）

- **🔵 FSは2階層(project/worktree)のまま維持**する。`WorktreeId=(usize,usize)` と `guess_names_from_cwd` が2階層前提で各所に浸透しており、ディレクトリ3階層化は破壊的変更となるため行わない。親子階層は `WorktreeMeta.parent` の**論理リンク**で表現する。
- **🔵 DBはスキーマ変更しない**。親子リンクの一次ソースは `project.json`（FSがsource-of-truth）。`messages` テーブルの既存 `message_type` 自由TEXT列を再利用し、新規テーブルは追加しない。
- **🔵 「指揮者」は独立エンティティにしない**。子を持つworktreeが指揮者（創発的）。指揮者化＝子を付ける/付け替えるだけの操作。

## 機能要件（EARS記法）

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 設計計画書・コンテキストノート・本ヒアリングを参考にした確実な要件
- 🟡 **黄信号**: 上記資料から妥当な推測による要件
- 🔴 **赤信号**: 上記資料にない推測による要件（設計フェーズでの再検討を推奨）

### Phase 0 — dispatch プリミティブ（自動投入の実証・最小の要）

親→1つの子へプロンプトを自動投入して起動させる、階層の唯一必要な基礎部品。

#### 通常要件

- **REQ-001**: システムは MCPツール `dispatch` を提供し、`target:{type:"worktree", id}` と `prompt` を受け取り、`messages` テーブルへ `message_type='dispatch'`, `content=prompt` としてINSERTしなければならない 🔵 *設計計画書 Phase0-2*
- **REQ-002**: システムは 既存の100ms `AppEvent::Tick` 処理内で未読の dispatch レコードを取得し、対象worktreeの tab0 の PTY へ `{prompt}\n` を書き込まなければならない 🔵 *設計計画書 Phase0-3*
- **REQ-003**: システムは dispatch のPTYへの書き込みが成功した場合にのみ、当該レコードを既読化しなければならない 🔵 *設計計画書 Phase0-3*

#### 条件付き要件

- **REQ-004**: 対象worktreeのセッション状態が working（実行中）の場合、システムは dispatch のプロンプトを待機させずPTYへ即時に書き込まなければならない 🔵 *ヒアリングQ1: 「PTYへ即時投入」を採用*
- **REQ-005**: 対象worktreeの Claude タブが未起動（PTY未生成）の場合、システムは当該dispatchを既読化せず、次回以降のTickでリトライしなければならない 🔵 *note.md注意事項、設計計画書Phase0-3*
- **REQ-006**: リトライ回数が上限（目安30回≒3秒、具体値は設計フェーズで確定）を超えた場合、システムは当該dispatchを既読化し、`set_alert` 相当の人間向けアラート通知を発報しなければならない 🟡 *ヒアリングQ2: 「一定回数リトライ後アラート」を採用、具体的な閾値は未確定*

#### 制約要件

- **REQ-007**: システムは dispatch を通常の保留メッセージ配信経路（SessionStart hook 経由 / `list_sessions` 呼び出し時の配信）から除外しなければならない（既存 `get_pending_messages` に `message_type != 'dispatch'` フィルタを追加し二重配信を防ぐ） 🔵 *note.md注意事項、設計計画書Phase0-1*
- **REQ-008**: システムは dispatch のプロンプト自動投入について、人間の承認ステップを設けず完全自動で投入しなければならない。ワーカー側の危険ツール実行は Claude Code CLI 自体が持つ組み込みの権限承認システム（siki hookとは独立して動作）で引き続きゲートされる 🔵 *ヒアリングQ3、設計フェーズのコード調査で訂正（下記参照）*

  > **🔴→🔵 訂正（設計フェーズのコード調査で判明）**: ヒアリングQ3時点では「siki側のPreToolUse hookが危険ツールをゲートする」という前提だったが、`src/hooks.rs:36-47` を確認した結果、siki の `PreToolUse` hook（`{siki_path} hook-event working`, `is_async: true`）は**セッション状態を`working`にする通知専用**であり、許可/拒否の判断（gating）は一切行っていないことが判明した。実際に危険ツール実行を止めているのはこの hook ではなく、**Claude Code CLI 自体が持つ組み込みの権限承認システム**（ツール実行前のユーザー承認プロンプト、siki hookとは無関係に常に動作）である。完全自動投入という結論（REQ-008採用）自体は変わらないが、安全根拠の記述をこのように訂正する。
- **REQ-009**: 対象worktreeが複数のClaudeタブを持つ場合、システムは常に tab0 へdispatchを投入しなければならない（タブ選択機能は本要件のスコープ外） 🔵 *設計計画書 スコープ外項*

### Phase 1 — worktree 階層モデル（論理オーバーレイ + 付け替え）

FSは2階層を維持したまま、親子を`project.json`の論理リンクで表現し、左ペインにツリー描画する。

#### 通常要件

- **REQ-010**: システムは `WorktreeMeta` に `parent: Option<String>`（同一project内の親worktree名）を追加し、既存の `load_project_meta`/`save_project_meta` で読み書きしなければならない 🔵 *設計計画書 Phase1-1*
- **REQ-012**: システムは MCPツール `move_worktree`（または `set_parent`）を提供し、`{child, parent}` を受け取って親子関係を付け替えられなければならない 🔵 *設計計画書 Phase1-3*
- **REQ-013**: システムは TUI左ペインのキー操作からも、子worktreeを指揮者配下へ移動する付け替え操作ができなければならない 🔵 *設計計画書 Phase1-3*
- **REQ-014**: システムは左ペインにおいて、親worktreeとその子孫worktreeを親子DFS順のツリー構造（深さに応じたインデント・罫線・兄弟内の最終判定）で描画しなければならない 🔵 *設計計画書 Phase1-4*

#### 状態要件

- **REQ-015**: 付け替え対象の新しい親が、付け替え対象の子worktree自身またはその子孫worktreeである場合（循環）、システムは付け替えを拒否しなければならない 🔵 *設計計画書 Phase1-1 循環参照ガード*
- **REQ-016**: 親worktreeが削除された場合、システムは当該worktreeを親として持つすべての子worktreeの `parent` を自動的に `None` に更新し、独立worktreeとして扱わなければならない 🔵 *ヒアリングQ4: 「親リンクをnullにして独立化」を採用*

#### 制約要件

- **REQ-011**: システムは親子リンクの表現にディレクトリ階層の変更を用いてはならず、`project.json` の `WorktreeMeta.parent` 論理リンクのみで表現しなければならない 🔵 *note.md確定制約1*

### Phase 2 — subtree 一斉指示 + 子状況の集約

親が配下全員に指示でき、配下の状態を吸い上げられるようにする。

#### 通常要件

- **REQ-017**: システムは `dispatch` の `target.type` に `"subtree"`（または`"children"`）を追加し、指定した親worktreeの子孫すべてへ一括でdispatchできなければならない 🔵 *設計計画書 Phase2-1*
- **REQ-018**: システムは親リンクを再帰的に辿り、対象worktreeの子孫一覧を解決するヘルパー（`get_descendants`）を提供しなければならない 🔵 *設計計画書 Phase2-1*
- **REQ-019**: システムは `list_sessions` に `scope:"children"|"subtree"` を追加し、指揮者worktreeが自身の子孫worktreeのセッション一覧のみに絞って取得できなければならない 🔵 *設計計画書 Phase2-2*
- **REQ-020**: システムは左ペインにおいて、親worktreeのバッジを子孫worktreeの状態集約（`aggregate_state` の優先度による畳み込み、アラートはOR）でロールアップ表示しなければならない 🔵 *設計計画書 Phase2-3*

#### 条件付き要件

- **REQ-021**: subtree dispatch実行時、一部の子worktreeへの投入が失敗（PTY未生成等）した場合でも、システムは成功した子への投入を妨げてはならず、失敗した子のみPhase0のリトライ機構（REQ-005, REQ-006）に従い個別にリトライしなければならない 🟡 *Phase0リトライ機構のsubtreeへの適用、既存資料に明記なし・妥当な推測*

### Phase 3 — 指揮者による子生成 + loop 運用パターン

#### 通常要件

- **REQ-022**: システムは既存の `finalize_add_worktree` を状態レス化した内部関数（`create_worktree_internal(app, pi, wt_name, branch, parent, ...)`）として切り出し、TUIのポップアップ状態に依存せず呼び出せるようにしなければならない 🔵 *設計計画書 Phase3-1*
- **REQ-023**: システムは MCPツール `spawn_child_worktree({parent, branch})` を提供し、指揮者Claudeが自身配下に子worktreeを生成でき、生成直後に親リンクを `project.json` へ保存しなければならない 🔵 *設計計画書 Phase3-1*

#### オプション要件

- **REQ-024**: 指揮者worktreeは `/loop` を用いて反応的に子状況をポーリングし、完了報告の受信やPRコメントへの対応をトリガーに次のdispatchを行ってもよい 🔵 *設計計画書 Phase3-2、note.md「loopの居場所」*
- **REQ-025**: ワーカーworktreeはdispatchで起動し、タスク完了後はidleに戻ってよく、loopを回す必要はない（トークン消費ゼロで待機） 🔵 *設計計画書 Phase3-2、note.md確定制約5*

## 非機能要件

### パフォーマンス

- **NFR-001**: dispatch配送は既存のTUI 100ms Tickサイクル内で処理しなければならず、これとは別に新規の定期ポーリング機構を追加してはならない 🔵 *設計計画書 Phase0-3、既存Tick再利用方針*

### データ整合性

- **NFR-002**: システムはDBスキーマを変更してはならず、既存 `messages` テーブルの `message_type` 列と `project.json` の拡張のみで親子階層・dispatch機構を実現しなければならない 🔵 *note.md確定制約2*

### セキュリティ

- **NFR-101**: dispatchによる完全自動投入であっても、ワーカー側の危険ツール実行（削除・force push等）は Claude Code CLI 自体の組み込み権限承認システムで引き続きゲートされ、バイパスしてはならない。siki の `PreToolUse` hook（`src/hooks.rs:36-47`）はセッション状態通知専用（`is_async: true`）でありツールの許可/拒否は行わないため、本要件のdispatch機構がこの承認システムを迂回しないことを設計フェーズで確認する 🔵 *ヒアリングQ3の安全根拠、設計フェーズのコード調査で訂正*

### ユーザビリティ

- **NFR-201**: 左ペインのツリー表示は、既存の罫線・インデント表現規約（`left_panel.rs`）を踏襲しなければならない 🟡 *既存実装からの妥当な推測*

## Edgeケース

### エラー処理

- **EDGE-001**: dispatch対象のworktreeが存在しない、または削除済みの場合、システムは当該dispatchを既読化せず失敗として扱い、REQ-006のアラート機構に従い人間に通知しなければならない 🟡 *REQ-006からの妥当な推測*
- **EDGE-002**: 親worktree削除によるREQ-016の `parent=None` 更新処理中に、対象の子worktree自体が同時に削除される競合が発生した場合の挙動は、本要件では規定せず設計フェーズで詳細化する 🔴 *既存資料に記載なし、要設計確認*

### 境界値

- **EDGE-101**: リトライ回数がREQ-006の上限値ちょうどに達した場合、その回でアラートを発報し既読化する。上限値+1回目のTickでは当該dispatchへの再試行を行わない 🟡 *REQ-006からの妥当な推測*
- **EDGE-102**: 親子関係はプロジェクトをまたいで（cross-project）設定できない。異なるprojectのworktreeを親または子として指定する `move_worktree`/`spawn_child_worktree` 呼び出しは拒否しなければならない 🔵 *設計計画書 スコープ外項*

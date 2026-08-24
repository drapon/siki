# 指揮者階層アーキテクチャ コンテキストノート

**作成日**: 2026-07-01
**要件名**: 指揮者階層アーキテクチャ（conductor-hierarchy）
**作業規模**: フル機能開発（Phase 0〜3を一括で要件化）

## 一次資料（信頼性の基準）

- 🔵 設計計画書: `~/.claude/plans/quizzical-frolicking-gizmo.md`（Phase 0〜3構成、本会話で合意）
- 🔵 本会話でのユーザー合意（3ユースケース・可変ツリー・付け替え・指揮者loop・完全自動投入・子Stop報告）
- 🔵 コード調査（3体のExploreエージェントによる実地調査、以下ファイル）

## 技術スタック

- 言語: Rust (edition 2024), siki v0.1.43
- TUI: ratatui + tokio + crossterm
- ターミナル: `portable-pty` + `vt100`（自前ターミナルエミュレータ、tmux/cmux不使用）
- 永続化: SQLite `~/.siki/siki.db`（`rusqlite`）+ ファイルシステム（`~/.siki/workspaces/<project>/<worktree>/`）+ `project.json`
- IPC: Unix ドメインソケット `~/.siki/sock`（状態hook受信専用）
- MCP: stdio短命プロセス（`siki mcp`、セッションごとに起動）

## 関連実装（変更・拡張の接続点）

| 領域 | ファイル:行 | 役割 |
|------|-----------|------|
| メッセージDB | `src/db.rs:34-45` (messages), `:270-301` (get_pending_messages), `:321-336` (mark_read) | dispatch記録・取得・既読 |
| MCPツール | `src/mcp/tools.rs:149-185` (send_message), `:164-169` (target振り分け) | 新ツールの雛形 |
| MCP登録 | `src/mcp/mod.rs:71` (instructions), schema一覧 | 新ツール登録 |
| TUIポーリング | `src/main.rs:1677` (AppEvent::Tick), `:1708` (アラート同期) | dispatch配送役の追加点 |
| PTY注入 | `src/terminal.rs:231` (write), `src/main.rs:4293-4307` (grep`s`投入実例), `:4298` (claude_terms取得) | プロンプト投入の低レベル手段 |
| worktree永続メタ | `src/config.rs:342-346` (WorktreeMeta), `:245-266` (save), `:447` (load), `:385` (migrate実績) | parentフィールド追加先 |
| インメモリworktree | `src/app.rs:228-255` (Worktree), `:698` (from_config), `:11` (WorktreeId=(usize,usize)) | parent反映 |
| 左ペイン描画 | `src/ui/left_panel.rs:31-46` (build_entries), `:131-160` (罫線/prefix), `:138-139` (バッジ) | ツリー描画・状態ロールアップ |
| worktree作成 | `src/main.rs:2122-2222` (finalize_add_worktree), `src/git.rs:36-` (create_worktree) | 状態レス化→MCP子生成 |
| 状態集約 | `src/session.rs:213-219` (aggregate_state), `:203-208` (by_worktree), `:398-411` (guess_names_from_cwd) | 配下集約 |
| 状態hook | `src/hooks.rs:36-47` (hook注入), `src/broker.rs:54-129` (受信), `src/session.rs:342-388` (更新) | 子Stop→親報告の起点 |

## 設計上の確定制約（🔵）

1. **FSは2階層(project/worktree)のまま維持**。`WorktreeId=(usize,usize)` と `guess_names_from_cwd` が2階層前提で各所に浸透しており、ディレクトリ3階層化は破壊的。階層は `WorktreeMeta.parent` の**論理リンク**で表現する。
2. **DBは状態キャッシュのまま**。親子リンクの一次ソースは `project.json`（FSがsource-of-truth）。新テーブルは原則不要（構造化進捗の保存先はDB拡張を検討）。
3. **「指揮者」は独立エンティティにしない**。子を持つworktreeが指揮者（創発的）。指揮者化＝子を付ける/付け替えるだけ。
4. **親なし(独立)worktreeとの区別が必須**。dispatch対象・完了報告は親子関係の有無で分岐する。
5. **loopの居場所**: ワーカー=dispatchで起きてidle(loop不要)。指揮者=loopで反応的（進捗ポーリング・完了報告受信・次dispatch）。

## 双方向アーキテクチャ

- **下り（親→子）**: dispatch（`messages` type='dispatch'）→ TUIポーリング → 対象PTYへ自動投入。
- **上り（子→親）**: 子のStop hook → 親が存在すれば親へ構造化完了報告 + 構造化進捗の集約。

## 注意事項

- dispatchは通常の保留メッセージ配信（SessionStart/list_sessions）から除外（二重配信防止）。
- PTY未生成の対象へのdispatchは既読化せず次ポーリングでリトライ（ただし無限リトライ回避策が要る）。
- 親削除時のdangling parent、循環付け替えのエッジケース対応が必要。
- 複数Claudeタブ時は当面tab0固定（将来タブ選択）。

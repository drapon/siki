# worktree削除時のインデックス整合性修正 コンテキストノート

**作成日**: 2026-07-01

## 技術スタック

- 言語: Rust (edition 2024)
- TUI: `ratatui` 0.30 / `crossterm` 0.29（event-stream）
- PTY: `portable-pty` 0.9 + `vt100` 0.16（`tui-term` 0.3 で描画）
- 非同期ランタイム: `tokio` 1.49（full features）。PTY 読み取りは `std::thread::spawn`、Claude Code の
  JSON ストリーム読み取りは `tokio::spawn` の非同期タスク
- DB: `rusqlite`（bundled）でブローカー用 SQLite（セッション状態・会話ログ）
- Git 操作: `git2`
- テスト: 標準 `#[test]` / `#[tokio::test]`（`src/main.rs` 末尾 `mod tests`）

## プロジェクト構造（関連部分）

```
src/
  app.rs        # App 状態、Worktree/Project 構造体、WorktreeId 型、worktree_by_id(_mut)
  main.rs       # イベントループ、worktree/project 削除ハンドラ、reindex_worktree_maps、
                #   resolve_claude_term_key、TerminalOutput/ClaudeOutput 等のイベント処理
  terminal.rs   # TerminalEmulator（PTY）。id: u64 をグローバル採番し spawn 時にクロージャへ
                #   worktree_id/tab_index をキャプチャして event 送信
  claude.rs     # ClaudeSession（`claude` CLI を stream-json モードで起動する別系統の Claude 実行経路）
  event.rs      # AppEvent 定義（TerminalOutput/TerminalExited/ClaudeOutput/ClaudeComplete/ClaudeError 等）
  git.rs        # WorktreeManager::remove_worktree（実際の `git worktree remove` 実行）
```

## 型・データモデルの要点

- `type WorktreeId = (usize, usize)`（`app.rs:11`) = `(project_index, worktree_index)`。
  `app.projects: Vec<Project>` / `Project.worktrees: Vec<Worktree>` への **直接インデックス**。
- `TerminalKey = (WorktreeId, usize)`（`main.rs:30`) をキーに `terminals` / `claude_terms` の
  `HashMap` を管理。Claude タブは `tab_index >= CLAUDE_TAB_BASE`（`usize::MAX - 100`）で判別。
- `sessions: HashMap<WorktreeId, claude::ClaudeSession>` は `send_to_claude`
  （`main.rs:2228`）が使う別系統の Claude 実行セッション。
- `projects`/`worktrees` は常に末尾へ `push` のみ（`main.rs:1801`, `main.rs:2160`）。削除は
  `Vec::remove` の一点のみ（worktree: `main.rs:4544`、project: `main.rs:4460`）。

## 関連実装（今回の調査で特定した箇所）

| ファイル:行 | 内容 |
|---|---|
| `src/main.rs:4486-4588` | `handle_archive_confirm_key`（worktree 削除）。削除後に `reindex_worktree_maps` を呼ぶ |
| `src/main.rs:4592-4634` | `reindex_worktree_maps`。同一プロジェクト内の worktree_index シフトのみ対応 |
| `src/main.rs:4417-4483` | `handle_remove_project_confirm_key`（プロジェクト削除）。**後続プロジェクトの reindex が存在しない**（Bug C） |
| `src/terminal.rs:113-200` | `TerminalEmulator::spawn_internal`。`id: u64`（グローバル採番, `NEXT_TERMINAL_ID`）と
  `worktree_id`/`tab_index` を reader thread のクロージャにキャプチャし、`TerminalOutput`/`TerminalExited` に乗せ続ける |
| `src/main.rs:41-56` | `resolve_claude_term_key`。tab_index のシフト（同一 worktree 内の Ctrl+W 起因）には対応するが、
  フォールバックが `key.0 == worktree_id` に限定されており worktree_id 自体のシフトには対応できない（Bug A の核心） |
| `src/main.rs:716-777` | `TerminalOutput`/`TerminalExited` 処理。Claude タブは上記 `resolve_claude_term_key` 経由、
  通常ターミナルは `terminals.get_mut(&(worktree_id, tab_index))` の**直接キー引きのみ**（再探索なし） |
| `src/claude.rs:11-135` | `ClaudeSession`。`id` フィールドなし。読み取りタスクが `worktree_id` をそのまま
  `ClaudeOutput`/`ClaudeComplete`/`ClaudeError` に乗せる |
| `src/app.rs:611-684` | `worktree_by_id(_mut)`（検証なしの Vec 直接インデックス）と `handle_claude_output`。
  古い `worktree_id` でも「たまたま存在する」別 worktree に無検証で書き込んでしまう（Bug B の核心） |

## 開発ルール・制約

- `.claude/rules/siki.md`: このリポジトリは siki 自身が管理する worktree（自己ホスト）。
- ユーザー全体ルール: git commit/push 等の破壊的操作は `/commit` コマンド経由以外禁止。
  YAGNI・単一責任・段階的改善を重視。
- コーディングスタイル: イミュータブル指向、関数は小さく、ファイルは集中させる、深いネスト禁止。
- 完了前検証ルール: 「動くはず」で終わらせず、実行結果（テスト実行ログ・exit code）を確認してから完了を主張する。

## 注意事項

- `terminals`/`claude_terms`/`sessions` はいずれも `(usize, usize)` ベースの HashMap キーであり、
  worktree/project の Vec インデックスシフトの影響を直接受ける。**キーのリインデックス**（`reindex_worktree_maps`
  相当）と、**既に起動済みの非同期タスクが送るイベントのルーティング**は別問題であり、片方だけ直しても
  もう片方が原因で不具合が残る。
- `TerminalEmulator::id`（`terminal.rs:42,162`）は既存のグローバル一意カウンタで、`ClaudeSession` には
  同等の仕組みが存在しない。今回の修正方針はこの `id` を全経路で「実体特定の正」として使う。
- 本要件はコードのみで完結し、外部サービス・APIキー・インフラ等のユーザー事前準備は不要
  （`prep.md` は生成しない）。

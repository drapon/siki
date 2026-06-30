# siki 操作系 CLI 要件定義書（軽量版）

## 概要

現状の siki CLI は `list` / `mcp` / `session-start` / `hook-event` と補助フラグのみで、
worktree の作成・削除・セッション起動といった操作系は全て TUI（`src/app.rs` のキーハンドラ）の中にある。
本要件は **siki の規約（worktree を `~/.siki/workspaces/<project>/<name>/` にぶら下げる）はそのままに、
TUI を立ち上げずに CLI だけで worktree とセッションを回せるようにする** ことを目的とする。

スコープは **B**: worktree 管理（new/rm/path/list）＋ 非TUI でのセッション起動（run）。

## 関連文書

- **ヒアリング記録**: [💬 interview-record.md](interview-record.md)
- **実装プラン**: `~/.claude/plans/zesty-leaping-stream.md`（調査結果と実装方針の詳細）

## 主要機能要件

**【信頼性レベル凡例】**:
- 🔵 **青信号**: コード調査・ユーザヒアリングを参考にした確実な要件
- 🟡 **黄信号**: コード調査・ユーザヒアリングから妥当な推測による要件
- 🔴 **赤信号**: 推測による要件

### 必須機能（Must Have）

- REQ-001: システムは `siki new <project> <name> [--base <ref>]` で、対象プロジェクトに
  worktree を `~/.siki/workspaces/<project>/<name>/` として作成しなければならない 🔵
  *ユーザヒアリング2026-06-30 + `config::worktree_path`(src/config.rs:25) より*
- REQ-002: `siki new` のブランチ名は worktree 名（`<name>`）と同名とし、`--base` 省略時は
  `resolve_base_branch`（siki.json > config.toml > "origin/main"）を起点に no_track で切らなければならない 🔵
  *ユーザヒアリング2026-06-30 + TUI FromBase モード(src/main.rs:2008)より*
- REQ-003: システムは `siki run <project> <name> [--base <ref>] [--resume] [-- <claude引数>]` で、
  worktree ディレクトリにて Claude（`config::resolve_llm` で解決した LLM）を
  **ユーザーの実 TTY に直接 exec** で起動しなければならない（TUI/PTY/broker を介さない）🔵
  *ユーザヒアリング2026-06-30 + `launch_llm_with_args`(src/main.rs:4760)の実体分析より*
- REQ-004: `siki run` は対象 worktree が存在しない場合、REQ-001/002 相当の作成を行ってから
  起動しなければならない（`--base` も反映）🔵 *ユーザヒアリング2026-06-30より*
- REQ-005: システムは `siki rm <project> <name>` で対象 worktree を削除しなければならない 🔵
  *`git::WorktreeManager::remove_worktree`(src/git.rs:190) + TUI archive フロー(src/main.rs:4570)より*
- REQ-006: システムは `siki path <project> <name>` で worktree の絶対パスを stdout に出力しなければならない
  （cd 連携用）🔵 *ユーザヒアリング2026-06-30より*
- REQ-007: システムは `siki list [project]` で、任意のプロジェクト名による絞り込みに対応しなければならない
  （既存 `siki list` の拡張）🔵 *既存実装(src/main.rs:93) + ユーザヒアリングより*

### 動作の制約・前提

- REQ-401: `run` は LLM が `claude` の場合のみ `hooks::ensure_hooks_configured`(src/hooks.rs:9) で
  hook を注入しなければならない（TUI の `launch_llm_with_args` と同条件）🔵 *src/main.rs:4780 より*
- REQ-402: broker（セッション監視）が起動していなくても `run` は破綻してはならない。
  `session-start` hook は DB に直接書き込むため、後から TUI を開けば履歴・状態が復元される 🔵
  *`session_start::run`(src/session_start.rs) + `BROKER_CONNECT_TIMEOUT=2s` の graceful 設計より*
- REQ-403: プロジェクト名は完全一致で解決し、見つからない場合は利用可能なプロジェクト名を列挙した
  エラーを返さなければならない 🟡 *安全側の妥当な推測（既存 `-p` は prefix 一致だが破壊的操作のため完全一致とする）*
- REQ-404: `siki new` は既に同名 worktree が存在する場合、早期にエラーで終了しなければならない 🟡
  *妥当な推測（誤上書き防止）*

### スコープ外（今回やらないこと）

- ワンショット非対話実行（`siki run ... -p "プロンプト"` で結果だけ受け取る）🔵 *ユーザヒアリング2026-06-30: 対話のみ*
- `run` プロセス内での broker 起動・ライブ監視 🔵 *ユーザヒアリング2026-06-30: 監視なし・前面exec*

## 簡易ユーザーストーリー

### ストーリー1: TUI を開かずに作業ブランチを起こして即着手する

**私は** siki ユーザー **として**
**ターミナルから `siki run <project> <feature>` を打って、worktree 作成から Claude 起動まで一気に行いたい**
**そうすることで** 軽い作業のたびに TUI 全体を立ち上げる手間を省ける

**関連要件**: REQ-003, REQ-004, REQ-001, REQ-002

### ストーリー2: スクリプト/シェルから worktree を管理する

**私は** siki ユーザー **として**
**`siki new` / `siki path` / `siki rm` をシェル関数や alias に組み込みたい**
**そうすることで** `cd (siki path proj wt)` のように既存ワークフローへ統合できる

**関連要件**: REQ-001, REQ-006, REQ-005, REQ-007

## 基本的な受け入れ基準

### REQ-001/002: worktree 作成

**Given**: クリーンな git リポジトリのプロジェクトが siki に検出されている
**When**: `siki new <project> wt-a` を実行する
**Then**: `~/.siki/workspaces/<project>/wt-a/` が作成され、ブランチ `wt-a` が
`origin/main`（または resolve_base_branch 結果）起点で no_track で切られる

**テストケース**:
- [ ] 正常系: 一時 git リポジトリで `cmd_new` を呼ぶと worktree が生成され `git worktree list` に出る（単体テスト）
- [ ] 異常系: 同名 worktree が既存の場合エラー終了する（REQ-404）
- [ ] 異常系: 存在しないプロジェクト名でエラー＋候補列挙する（REQ-403）

### REQ-003/004: 非TUI セッション起動

**Given**: 対象 worktree が存在する（または存在しない）
**When**: `siki run <project> wt-a` を実行する
**Then**: （無ければ作成後）worktree dir で claude が実 TTY に exec され、対話できる

**テストケース**:
- [ ] 正常系: 既存 worktree で claude が当該 dir で起動する（手動E2E）
- [ ] 正常系: worktree 不在時に自動作成されてから起動する（手動E2E）
- [ ] 正常系: 別途 TUI を開くと当該セッションの状態/履歴が DB から復元される（手動E2E、REQ-402）

### REQ-005/006/007: 削除・パス・一覧

- [ ] `siki path <project> wt-a` が絶対パスを返す
- [ ] `siki rm <project> wt-a` で worktree が消える
- [ ] `siki list <project>` が当該プロジェクトのみ表示する

## 最小限の非機能要件

- **保守性**: 操作系の本体は新規 `src/cli.rs` に集約し、肥大化した `src/main.rs`（約330KB）を
  これ以上太らせない。`resolve_base_branch` は `config.rs` へ移して main/cli で共有する 🔵 *コーディング規約（多くの小さなファイル）より*
- **互換性**: 既存の TUI / broker / PTY コードには手を入れない（exec 方式でセッション多重化と非干渉）🔵
- **パフォーマンス**: broker 非起動時、`run` のたびに session-start hook が最大 2s で connect を諦める
  起動遅延が出るが許容する 🟡 *既存 graceful 設計から妥当な推測*

## 品質評価

🔵 青信号が主体（実コード調査＋ユーザヒアリングで裏取り済み）。曖昧さは小さく、実装可能性は確実。
**評価: 高品質**

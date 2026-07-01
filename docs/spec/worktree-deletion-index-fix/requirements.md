# worktree削除時のインデックス整合性修正 要件定義書

**作成日**: 2026-07-01
**作業規模**: フル機能開発
**対象**: siki TUI のイベントルーティング（`src/main.rs` / `src/terminal.rs` / `src/claude.rs` / `src/app.rs`）

## 概要

siki では `type WorktreeId = (usize, usize)`（`project_index, worktree_index`）を使って
`app.projects[pi].worktrees[wi]` を直接インデックスし、同じ添字を `sessions`/`terminals`/
`claude_terms` の `HashMap` キーにも流用している。worktree やプロジェクトを削除すると
`Vec::remove` で後続要素の添字がすべて 1 つ前へシフトするため、既存実装は削除確定時に
`HashMap` のキーを付け替える `reindex_worktree_maps`（worktree 単位のみ）を持っている。

しかし、この reindex は **HashMap のキー** を直すだけで、**既に起動済みの PTY 読み取りスレッド／
Claude Code ストリーム読み取りタスクが spawn 時にクロージャへキャプチャした `worktree_id`・
`tab_index`** までは追従できない。これらのバックグラウンドタスクは生存中ずっと古い識別子を
イベントに乗せ続けるため、キー（実体の保存場所）とイベントのタグ（送信元の自己申告）が食い違い、
以下の実害を生む。

- **Bug A**: worktree 削除後、それより後ろにいた別 worktree の既に開いている PTY/Claude タブが
  出力更新を受け取れず「フリーズ」する（`resolve_claude_term_key` のフォールバックが
  `key.0 == worktree_id` に限定されており、worktree_id 自体のシフトを救えないため）
- **Bug B**: `send_to_claude`/`ClaudeSession` 経由のチャット履歴更新は id 照合が一切なく、
  古い `worktree_id` がシフト後にたまたま指す**別の実在 worktree**へ無検証で書き込まれ、
  内容が差し替わる
- **Bug C**（追加発見）: プロジェクト削除（`handle_remove_project_confirm_key`）には
  worktree 削除の `reindex_worktree_maps` に相当する後続プロジェクトの reindex が存在せず、
  同種の問題がプロジェクト単位でも発生しうる

本要件では、「HashMap キーの reindex」に加えて、**TerminalEmulator が既に持つグローバル一意 id
を `ClaudeSession` にも導入し、イベント側は worktree_id/tab_index を「ヒント」として使い、
id 一致で実体（と、実体が現在保存されているキー）を確定する」方式**でこれらを解消する。

## 関連文書

- **ヒアリング記録**: [💬 interview-record.md](interview-record.md)
- **ユーザストーリー**: [📖 user-stories.md](user-stories.md)
- **受け入れ基準**: [✅ acceptance-criteria.md](acceptance-criteria.md)
- **コンテキストノート**: [📝 note.md](note.md)
- **PRD**: なし（直前の会話でのコード調査 + ユーザヒアリングをソースとする）

## 機能要件（EARS記法）

**【信頼性レベル凡例】**:
- 🔵 **青信号**: コード調査（本リポジトリの実装確認）またはユーザヒアリングで確定した要件
- 🟡 **黄信号**: コード調査・ヒアリングから妥当な推測による要件
- 🔴 **赤信号**: コード調査・ヒアリングにない推測による要件

### 通常要件

- **REQ-001**: システムは `TerminalEmulator` について、生成時点でグローバルに一意な `id` を
  採番し、生存期間中不変に保持しなければならない 🔵 *既存実装 (`terminal.rs:42,162`) の維持要件*
- **REQ-002**: システムは `ClaudeSession`（`claude.rs`）に対しても、`TerminalEmulator::id` と
  同様の生成時点で採番されるグローバル一意 `id` を新規に付与しなければならない
  🔵 *ヒアリングで修正方針として確定*
- **REQ-003**: `AppEvent::TerminalOutput`/`TerminalExited` を受信した際、システムはまず
  イベントに乗った `(worktree_id, tab_index)` をヒントとして対象エンティティを検索し、
  一致しない場合は `terminal_id` が一致するエンティティを対象 `HashMap` **全体**から
  再探索しなければならない 🔵 *コード調査 (`main.rs:41-56,716-777`) + ヒアリングで確定*
- **REQ-004**: `resolve_claude_term_key` の再探索条件から `key.0 == worktree_id` という
  worktree 単位の絞り込みを撤廃し、`claude_terms` 全体を `terminal_id` のみで走査するよう
  変更しなければならない 🔵 *コード調査 (`main.rs:52-55`) で特定した修正箇所*
- **REQ-005**: 通常ターミナル（`terminals`）についても、Claude タブと同じ考え方の id ベース
  再探索ヘルパーを新設し、`TerminalOutput`/`TerminalExited` の処理で使用しなければならない
  （現状は直接キー引きのみで再探索処理が存在しない）🔵 *コード調査 (`main.rs:747-751,772-776`) で確定*
- **REQ-006**: `AppEvent::ClaudeOutput`/`ClaudeComplete`/`ClaudeError` の処理は、`sessions`
  から `id` 一致で対象 `ClaudeSession` を再探索し、その際に見つかった「現在の HashMap キー」
  を worktree 特定に用いて `app.worktree_by_id_mut`/`app.worktree_by_id` を呼び出さなければ
  ならない。イベントに乗った古い `worktree_id` を worktree 特定に直接使ってはならない
  🔵 *コード調査 (`app.rs:611-627`) で確認した無検証 Vec インデックスアクセスの是正*

### 条件付き要件

- **REQ-101**: worktree 削除（`handle_archive_confirm_key`）により同一プロジェクト内の
  後続 worktree の index がシフトした場合、既存の `reindex_worktree_maps` によって
  `sessions`/`terminals`/`claude_terms` のキーは削除位置以降 1 つ前方へシフトされなければ
  ならない（既存動作を回帰させない）🔵 *既存実装 (`main.rs:4556,4592-4634`) の維持要件*
- **REQ-102**: プロジェクト削除（`handle_remove_project_confirm_key`）により後続プロジェクト
  の index がシフトした場合、システムは新設する `reindex_project_maps` によって、削除位置
  より後ろの `project_index` を持つ `sessions`/`terminals`/`claude_terms` のキーを 1 つ前方へ
  シフトしなければならない 🔵 *コード調査 (`main.rs:4417-4483`) で確認した未実装箇所（Bug C）*
- **REQ-103**: プロジェクト削除時、`app.selected_worktree` が削除対象プロジェクトより後ろの
  プロジェクトを指していた場合、`project_index` を 1 つ前方へ補正しなければならない
  （既存実装 `main.rs:4463-4469` を維持）🔵 *既存実装の維持要件*

### 状態要件

- **REQ-201**: バックグラウンドタスク（PTY 読み取りスレッド／`ClaudeSession` ストリーム
  読み取りタスク）が生存している間、そのタスクが送信するすべてのイベントには spawn 時に
  採番した不変の `id` を含めなければならない 🔵 *コード調査 (`terminal.rs:162-200`, `claude.rs`) より*
- **REQ-202**: worktree/プロジェクトの削除操作が確認ダイアログ表示中である間、システムは
  対象の `sessions`/`terminals`/`claude_terms` エントリを変更してはならない（削除確定後にのみ
  クリーンアップ・reindex を行う）🔵 *既存実装踏襲*

### 制約要件

- **REQ-401**: `id` 一致による再探索を行っても対象エンティティが見つからない場合
  （実際にタブが閉じられた等）、システムは当該イベントを黙って破棄しなければならない
  （エラー表示・ログ出力は追加しない）🔵 *ヒアリングで現行動作の維持を確定*
- **REQ-402**: `archive_target`/`rename_worktree_target`/`session_choice_wt_id`/
  `llm_picker_wt_id` 等のモーダル系 `WorktreeId` 保持フィールドは今回のスコープ対象外とし、
  変更を加えない 🔵 *ヒアリングでスコープ外と確定*
- **REQ-403**: 本修正は `AppEvent`（`TerminalOutput`/`TerminalExited`/`ClaudeOutput`/
  `ClaudeComplete`/`ClaudeError`）や `ClaudeSession` 構造体への新規フィールド追加を伴って
  よい（内部プロトコルであり外部互換性維持は不要）🔵 *ヒアリングで許容を確定*

## 非機能要件

### パフォーマンス

- **NFR-001**: `id` 一致による `HashMap` 全体スキャンは、README 記載の実利用規模
  （1 worktree あたり最大 5 ターミナルタブ + Claude タブ、`README.md` "Up to 5 terminal
  tabs per worktree"）を踏まえると全 worktree 合計でも数十エントリ程度に収まるため、
  線形走査によるオーバーヘッドは無視できるレベルでなければならない
  🟡 *README記載の利用規模からの妥当な推測*

### 保守性

- **NFR-101**: 新設する id 解決ヘルパー（`terminals`/`claude_terms`/`sessions` 用）は
  共通化可能な部分をまとめ、将来同種のマップが増えても同じパターンで再利用できる設計と
  しなければならない 🟡 *設計方針からの妥当な推測*

## Edgeケース

### エラー処理

- **EDGE-001**: 同一 worktree 内で Claude タブを Ctrl+W で閉じたことによる `tab_index` シフト
  と、worktree 削除による `worktree_id` シフトが同時に絡むケースでも、id 一致による解決は
  正しく機能しなければならない 🟡 *既存 `resolve_claude_term_key` のコメント (`main.rs:36-40`)
  を踏まえた妥当な推測*
- **EDGE-002**: プロジェクト削除と worktree 削除が短時間に連続して発生した場合でも、各削除
  確定処理内で reindex が同期的に完了するため競合状態は発生してはならない（`git worktree`
  の実削除自体は `tokio::task::spawn_blocking` で非同期だが、インメモリの reindex はメイン
  スレッド上で同期的に行う既存の設計を維持する）🔵 *既存実装 (`main.rs:4556,4566-4576`) の
  スレッドモデルより*

### 境界値

- **EDGE-101**: 削除対象が worktree 一覧の先頭（`wi=0`）または末尾（最終 `wi`）の場合でも、
  reindex ロジックは正しく動作しなければならない（境界値で off-by-one を作らない）
  🔵 *既存実装のロジック確認要件*
- **EDGE-102**: プロジェクトが1つしかない状態でそのプロジェクトを削除した場合、後続プロジェクト
  が存在しないため `reindex_project_maps` は何もせず正常終了しなければならない
  🔵 *境界値の妥当性確認*

## 参考: 実装対象（`/tsumiki:kairo-design` 向けメモ）

| ファイル | 変更概要 |
|----------|----------|
| `src/terminal.rs` | 変更なし（既存 `id: u64` をそのまま活用） |
| `src/claude.rs` | `ClaudeSession` に `id: u64`（グローバル採番）を追加。読み取りタスクが送る
  `ClaudeOutput`/`ClaudeComplete`/`ClaudeError` に `session_id` を付与 |
| `src/event.rs` | `AppEvent::ClaudeOutput`/`ClaudeComplete`/`ClaudeError` に `session_id: u64`
  フィールドを追加 |
| `src/main.rs` | `resolve_claude_term_key` の絞り込み撤廃（REQ-004）、`terminals` 用の同等
  ヘルパー新設（REQ-005）、`sessions` 用の id 解決ヘルパー新設と `ClaudeOutput` 等ハンドラの
  書き換え（REQ-006）、`handle_remove_project_confirm_key` への `reindex_project_maps` 追加
  （REQ-102） |
| `src/app.rs` | 変更なし（`worktree_by_id_mut` 自体はそのまま。呼び出し側が解決済みキーを渡す） |

依存追加なし。すべて既存クレート（`std::sync::atomic::AtomicU64` 等）で実装可能。

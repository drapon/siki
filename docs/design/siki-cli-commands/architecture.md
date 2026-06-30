# siki 操作系 CLI アーキテクチャ設計

**作成日**: 2026-06-30
**関連要件定義**: [requirements.md](../../spec/siki-cli-commands/requirements.md)
**ヒアリング記録**: [design-interview.md](design-interview.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 要件定義・既存コード・ユーザヒアリングを参考にした確実な設計
- 🟡 **黄信号**: 妥当な推測による設計
- 🔴 **赤信号**: 根拠の薄い推測による設計

---

## システム概要 🔵

**信頼性**: 🔵 *requirements.md・ユーザヒアリング2026-06-30より*

siki に、TUI を起動せずに worktree とセッションを操作する CLI サブコマンド群を追加する。
既存の TUI / broker / PTY には手を入れず、TUI 内のロジック（`launch_llm_with_args` 等）が
依存していた**純粋関数**（`config` / `git` / `hooks`）を CLI から薄く呼び出す。

追加コマンド: `new` / `rm` / `path` / `run` / `list`（既存 list の拡張）。

## 設計の中心: 2モード方式（宣言 / 対話フォールバック）🔵

**信頼性**: 🔵 *ユーザヒアリング2026-06-30より*

`run` / `new` / `rm` は、引数の充足度で2モードに分岐する。

- **宣言モード**: 必要な引数が揃っていれば、その通りに非対話で実行する
  （例: `siki run myapp feature --base origin/dev -- --model opus`）。スクリプト/CI に向く。
- **対話フォールバック**: 不足分のみ、矢印キー型セレクタ/入力欄で対話的に補完する
  （例: `siki run` だけ → プロジェクト選択 → worktree 選択 or 新規名入力 → base 入力）。

この方針の帰結として、**位置引数は必須にしない**（不足を検知して対話に回す）。
`path` / `list` はスクリプト連携（`cd (siki path ...)` 等）用途のため**非対話**を維持し、
stdout を選択 UI で汚さない。

## アーキテクチャパターン 🔵

**信頼性**: 🔵 *coding-style.md（小さなファイル/単一責任）・既存コード構成より*

- **パターン**: 既存の「`main.rs` ディスパッチ + ドメイン別モジュール（config/git/hooks/...）」を踏襲。
- **新規追加は薄い CLI 層に閉じ込める**。巨大化した `src/main.rs`（約330KB）をこれ以上太らせない。

### モジュール構成 🔵

**信頼性**: 🔵 *coding-style.md・ユーザヒアリングより*

```
src/
├── main.rs            … 既存ディスパッチに new/rm/path/run と list 拡張を追記（数行）
├── cli/
│   ├── mod.rs         … サブコマンドのエントリ（cmd_new/cmd_rm/cmd_path/cmd_run/cmd_list）
│   ├── args.rs        … 0依存の小さな引数スキャナ ArgScan（位置/値フラグ/真偽フラグ/-- 丸投げ）
│   └── prompt.rs      … crossterm ベースの対話セレクタ/入力欄（新依存なし）
├── config.rs          … resolve_base_branch を main.rs から移設（main/cli で共有）
├── git.rs             … create_worktree_from_ref / remove_worktree（既存・変更なし）
└── hooks.rs           … ensure_hooks_configured（既存・変更なし）
```

**決定: 引数パースは「手書き＋小さな共有ヘルパ（ArgScan）」**🔵
*ユーザヒアリング2026-06-30より*。clap 等は導入しない。理由: 対話フォールバックにより
「必須引数の自動エラー / --help 自動生成」という clap の主利点が薄れ、新依存・既存ディスパッチ
全面移行のコストに見合わないため。値フラグ・`--` 丸投げの検証ロジックを `ArgScan` 1箇所に集約する。

## プロセスモデル: exec 置換 🔵

**信頼性**: 🔵 *ユーザヒアリング2026-06-30より*

`run` は最終段で `std::os::unix::process::CommandExt::exec` により siki プロセスを LLM（既定 `claude`）
に**置き換える**。siki は unix 専用（unix socket 使用）のため移植性問題はなく、余分な親プロセスが
残らず exit code も LLM のものになる。対話セレクタは exec の**前**に完結させ、crossterm の raw mode を
解除してから exec する（端末状態を残さない）。

## broker 非起動時の挙動 🔵

**信頼性**: 🔵 *session_start.rs・hook_event.rs の既存タイムアウト設計より*

CLI 起動セッションは broker（TUI 内常駐）が無くても破綻しない。
`siki run` で `claude` を起動すると注入済み hook が動き、`session-start` hook は **DB に直接書き込む**ため、
後から TUI を開けば履歴・`claude_session_id` が復元される。broker への接続は
`BROKER_CONNECT_TIMEOUT=2s` で graceful に打ち切られる（最悪 2s の起動遅延、許容）。

## 非機能要件の実現方法

### 保守性 🔵

**信頼性**: 🔵 *coding-style.md より*

- CLI 本体を `src/cli/` 配下の小さなファイル群に分離（各 200–400 行目安）。
- `resolve_base_branch` を `config.rs` へ移し、main/cli の重複を排除。

### 互換性 🔵

**信頼性**: 🔵 *ユーザヒアリングより*

- TUI / broker / PTY のコードは不変更。exec 方式によりセッション多重化ロジックと非干渉。
- 既存の内部サブコマンド（`mcp` / `session-start` / `hook-event`）の引数形は据え置き。

### パフォーマンス 🟡

**信頼性**: 🟡 *既存 graceful 設計からの妥当な推測*

- broker 非起動時、`run` のたびに session-start hook が最大 2s で connect を諦める遅延が出るが許容。

## 技術的制約 🔵

**信頼性**: 🔵 *Cargo.toml・既存コードより*

- 言語: Rust 2024 edition。新規 crate 追加なし（crossterm/anyhow/git2 等は既存依存）。
- 対象 OS: unix（macOS/Linux）。`CommandExt::exec` と unix socket に依存。
- `git` CLI 前提（`git.rs` は `git worktree` をサブプロセス実行）。

## 関連文書

- **データフロー**: [dataflow.md](dataflow.md)
- **型/シグネチャ定義**: [interfaces.rs](interfaces.rs)
- **要件定義**: [requirements.md](../../spec/siki-cli-commands/requirements.md)
- **実装プラン**: `~/.claude/plans/zesty-leaping-stream.md`
- DB スキーマ / API エンドポイント: **非該当**（CLI であり Web/REST/DB スキーマ追加なし）

## 信頼性レベルサマリー

- 🔵 青信号: 12件 (86%)
- 🟡 黄信号: 2件 (14%)
- 🔴 赤信号: 0件 (0%)

**品質評価**: 高品質（要件ヒアリング＋設計ヒアリング＋実コード調査で裏取り済み）

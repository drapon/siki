# siki 操作系 CLI データフロー図

**作成日**: 2026-06-30
**関連アーキテクチャ**: [architecture.md](architecture.md)
**関連要件定義**: [requirements.md](../../spec/siki-cli-commands/requirements.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 要件定義・既存コード・ユーザヒアリングを参考にした確実なフロー
- 🟡 **黄信号**: 妥当な推測によるフロー
- 🔴 **赤信号**: 根拠の薄い推測によるフロー

---

## 全体ディスパッチ 🔵

**信頼性**: 🔵 *main.rs:64-149 の既存ディスパッチ構造より*

```mermaid
flowchart TD
    A["siki &lt;args&gt;"] --> B{args[1]}
    B -->|"--version / --help"| Z1[既存処理]
    B -->|"mcp / session-start / hook-event"| Z2[既存の内部サブコマンド]
    B -->|"list [project]"| L[cli::cmd_list]
    B -->|new| N[cli::cmd_new]
    B -->|rm| R[cli::cmd_rm]
    B -->|path| P[cli::cmd_path]
    B -->|run| RUN[cli::cmd_run]
    B -->|なし / -p| T[TUI 起動（既存）]
```

## 引数パース（ArgScan）🔵

**信頼性**: 🔵 *ユーザヒアリング2026-06-30（手書き＋共有ヘルパ）より*

```mermaid
flowchart TD
    A["args[2..]"] --> B{各トークン}
    B -->|"--"| C[以降を rest（passthrough）へ]
    B -->|"--key（値フラグ）"| D[次トークンを values[key] へ]
    B -->|"--flag（真偽）"| E[flags へ追加]
    B -->|"-で始まる未知"| F[エラー: unknown flag]
    B -->|それ以外| G[positionals へ追加]
    C & D & E & G --> H[ArgScan 構築]
    H --> I["positionals(n) で過不足検証 / value(k) / has(k) / rest()"]
```

## run: 2モード（宣言 / 対話フォールバック）🔵

**信頼性**: 🔵 *ユーザヒアリング2026-06-30より*
**関連要件**: REQ-003, REQ-004, REQ-401, REQ-402

```mermaid
flowchart TD
    A["siki run [project] [name] [--base] [--resume] [-- ...]"] --> B[ArgScan で解析]
    B --> C{project あり?}
    C -->|なし| C1[prompt: プロジェクト選択（矢印キー）]
    C -->|あり| D
    C1 --> D{name あり?}
    D -->|なし| D1[prompt: worktree 選択 or ＋新規]
    D1 --> D2{＋新規?}
    D2 -->|はい| D3[prompt: 新規名入力 + base 入力]
    D2 -->|いいえ| E
    D3 --> E
    D -->|あり| E{worktree 存在?}
    E -->|なし| F[cmd_new 相当で作成（--base 反映）]
    E -->|あり| G
    F --> G[config::resolve_llm]
    G --> H{llm == claude?}
    H -->|はい| H1[hooks::ensure_hooks_configured]
    H -->|いいえ| I
    H1 --> I[raw mode 解除・端末復帰]
    I --> J["CommandExt::exec(llm, args + (--resume?-r) + passthrough) @ worktree dir"]
    J --> K[(siki プロセスは claude に置換)]
```

### run 起動後の hook → DB/broker 連携 🔵

**信頼性**: 🔵 *session_start.rs・broker 非起動時の graceful 設計より*
**関連要件**: REQ-402

```mermaid
sequenceDiagram
    participant CLI as siki run (→ exec claude)
    participant Claude as claude プロセス
    participant Hook as siki session-start (hook)
    participant DB as SQLite
    participant Broker as broker (TUI、通常未起動)

    CLI->>Claude: exec（worktree dir, hook 注入済み）
    Claude->>Hook: SessionStart hook 実行
    Hook->>DB: セッション情報を直接書き込み（常に成功）
    Hook->>Broker: register 送信を試行
    alt broker 起動中（TUI が別途稼働）
        Broker-->>Hook: 受理（ライブ監視）
    else broker 未起動（CLI 単独）
        Note over Hook,Broker: connect を最大2sで打ち切り（graceful）
    end
    Note over DB: 後で TUI 起動時に履歴/claude_session_id を復元
```

## new: worktree 作成 🔵

**信頼性**: 🔵 *finalize_add_worktree(main.rs:2122) の非TUI部分の再現*
**関連要件**: REQ-001, REQ-002, REQ-403, REQ-404

```mermaid
flowchart TD
    A["siki new [project] [name] [--base]"] --> B[ArgScan]
    B --> C{project/name 揃う?}
    C -->|不足| C1[prompt で補完]
    C -->|揃う| D[discover_projects で解決<br/>無ければ候補列挙エラー REQ-403]
    C1 --> D
    D --> E[wt_path = config::worktree_path]
    E --> F{既存?}
    F -->|あり| F1[エラー終了 REQ-404]
    F -->|なし| G[base = --base or config::resolve_base_branch]
    G --> H[shared_dirs = load_effective_shared_dirs]
    H --> I["git::create_worktree_from_ref(branch=name, Some(base), no_track=true)"]
    I --> J{siki.json scripts.setup あり?}
    J -->|あり| J1[wt_path で setup を inherited-stdio 実行]
    J -->|なし| K[作成パスを表示]
    J1 --> K
```

## rm: worktree 削除 🔵

**信頼性**: 🔵 *git::remove_worktree(git.rs:190)・archive フロー(main.rs:4570)より*
**関連要件**: REQ-005

```mermaid
flowchart TD
    A["siki rm [project] [name]"] --> B[ArgScan]
    B --> C{project/name 揃う?}
    C -->|不足| C1[prompt で選択]
    C -->|揃う| D[wt_path 解決]
    C1 --> D
    D --> E[prompt: 削除確認（誤削除防止）]
    E -->|yes| F["git::remove_worktree(project_path, wt_path)"]
    E -->|no| G[中止]
    F --> H[結果表示]
```

## path / list: 非対話 🔵

**信頼性**: 🔵 *ユーザヒアリング（スクリプト連携のため非対話）より*
**関連要件**: REQ-006, REQ-007

- `path <project> <name>`: `config::worktree_path` を**絶対パスで stdout に出力**。不足/不在は stderr + 非0終了。
- `list [project]`: `discover_projects` を列挙。project 指定時はそのプロジェクトのみ。

## エラーハンドリング方針 🟡

**信頼性**: 🟡 *coding-style.md（包括的エラー処理）・anyhow 既存利用より*

```mermaid
flowchart TD
    A[各 cmd] --> B{Result}
    B -->|Ok| C[exit 0]
    B -->|Err| D["eprintln! でユーザー向けメッセージ"]
    D --> E[std::process::exit 非0]
```

- プロジェクト/worktree 未解決、`--base` の値欠落、既存 worktree への new 等は anyhow の `Result` で
  ユーザー向けメッセージにして非0終了。対話中の Esc は中止（非0 or 0 は実装時に統一）。

## 関連文書

- **アーキテクチャ**: [architecture.md](architecture.md)
- **型/シグネチャ定義**: [interfaces.rs](interfaces.rs)
- **要件定義**: [requirements.md](../../spec/siki-cli-commands/requirements.md)

## 信頼性レベルサマリー

- 🔵 青信号: 8件 (80%)
- 🟡 黄信号: 2件 (20%)
- 🔴 赤信号: 0件 (0%)

**品質評価**: 高品質

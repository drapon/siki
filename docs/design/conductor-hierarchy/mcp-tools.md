# 指揮者階層アーキテクチャ MCPツール仕様

**作成日**: 2026-07-02
**関連設計**: [architecture.md](architecture.md)
**関連要件定義**: [requirements.md](../../spec/conductor-hierarchy/requirements.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 要件定義書・実地コード調査（`mcp/protocol.rs:53-230`, `mcp/tools.rs:10-185`）を参考にした確実な定義
- 🟡 **黄信号**: 上記から妥当な推測による定義
- 🔴 **赤信号**: 上記にない推測による定義

> **本ファイルについて**: siki はREST APIではなくMCP（Model Context Protocol）のstdioサーバー（`siki mcp`）としてツールを公開する。本ドキュメントはkairo-designテンプレートの「API仕様」をMCPツール仕様に読み替えたものであり、`api-endpoints.md`テンプレートの代わりに本ファイルを使用する。

---

## 共通仕様

### ツール一覧・登録 🔵

**信頼性**: 🔵 *実地調査: `mcp/mod.rs:81-87`（`tools/list`→`protocol::tool_definitions()`）、`mcp/protocol.rs:63-230`（`ToolDefinition`定義本体）*

新規ツールのJSON Schemaは `src/mcp/protocol.rs` の `tool_definitions()` に追加する（**`mcp/mod.rs`ではない**、design-interview.md訂正3）。ディスパッチは `src/mcp/tools.rs` の `execute_tool()`（`tools.rs:10-29`）に新しい `match` アームを追加する。

### レスポンス形式 🔵

**信頼性**: 🔵 *既存 `send_message`/`broadcast` のレスポンス（`tools.rs:172-174, 183`）より*

既存ツールと同様、`serde_json::json!({...})` を直接返す軽量な形式。共通エラーラッパーは存在せず、`anyhow::Result<Value>` のErr内容がそのまま `isError:true` のテキストとして返る（`mcp/mod.rs:89-122`のtools/call実装）。

---

## 新規ツール

### dispatch 🔵

**信頼性**: 🔵 *requirements.md REQ-001〜009, REQ-017, REQ-018, REQ-021*

**説明**: 指揮者worktreeから子worktree（またはその配下全員）へプロンプトを送り、対象のClaude Code Codeターミナル（tab0）へ完全自動で投入させる。既存 `send_message` と異なり、投入は保留メッセージの手動確認を待たずTUIの100ms Tickで自動実行される。

**関連要件**: REQ-001, REQ-002, REQ-003, REQ-004, REQ-005, REQ-006, REQ-007, REQ-008, REQ-009, REQ-017, REQ-018, REQ-021

**入力スキーマ**（`send_message`の`target`パターンを踏襲、`protocol.rs:88`相当の位置に追加）:
```json
{
  "type": "object",
  "properties": {
    "target": {
      "type": "object",
      "properties": {
        "type": { "type": "string", "enum": ["worktree", "subtree"] },
        "id": { "type": "string", "description": "worktree名（typeがsubtreeの場合は指揮者worktree名）" }
      },
      "required": ["type", "id"]
    },
    "prompt": { "type": "string" }
  },
  "required": ["target", "prompt"]
}
```

**実装概要**（`tools.rs`内、新規関数）:
```rust
fn dispatch(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let target = params.get("target").ok_or_else(|| anyhow::anyhow!("target is required"))?;
    let target_type = target.get("type").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.type is required"))?;
    let target_id = target.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.id is required"))?;
    let prompt = params.get("prompt").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("prompt is required"))?;

    // 呼び出し元セッションの project_name を自己解決（list_sessionsと同じ自己参照パターン、tools.rs:38-42）
    let all_sessions = db::list_sessions(conn)?;
    let project_name = all_sessions.iter()
        .find(|s| s.session_id == from_session)
        .map(|s| s.project_name.clone())
        .ok_or_else(|| anyhow::anyhow!("caller session not found"))?;

    let targets: Vec<String> = match target_type {
        "worktree" => vec![target_id.to_string()],
        "subtree" => config::get_descendants(&project_name, target_id), // REQ-017, REQ-018
        _ => anyhow::bail!("Invalid target type: {}", target_type),
    };

    for wt_name in &targets {
        db::insert_message(
            conn, from_session, None, Some(wt_name), Some(&project_name),
            prompt, "dispatch", None,                                    // REQ-001, REQ-007
        )?;
    }

    Ok(json!({ "dispatched": targets.len(), "targets": targets }))
}
```

**戻り値**: `{"dispatched": <件数>, "targets": [<対象worktree名>...]}` 🟡 *既存ツールのレスポンス慣習から妥当な推測、確定仕様ではない*

**エラーケース**:
- `target.type`が`"worktree"`/`"subtree"`以外 → 同期エラー
- 呼び出し元セッションが特定できない → 同期エラー
- `target`が空配列（subtreeで子孫が存在しない） → `{"dispatched": 0, "targets": []}` を正常応答として返す 🟡

**cross-project制約**（EDGE-102）: `project_name`は常に呼び出し元セッション自身のprojectに固定されるため、異なるprojectのworktreeへdispatchすることは構造上不可能（target_idが他projectのworktree名と偶然一致していても、その名前が自project内の`get_descendants`/`project.json`に存在しなければ配送されない）🔵。

---

### move_worktree 🔵

**信頼性**: 🔵 *requirements.md REQ-012, REQ-013, REQ-015, EDGE-102、design-interview.md Q3*

**説明**: 子worktreeの親を付け替える（指揮者間の移動）。`parent`を省略/nullにすると独立worktree化する。

**関連要件**: REQ-012, REQ-015, EDGE-102

**入力スキーマ**:
```json
{
  "type": "object",
  "properties": {
    "child": { "type": "string", "description": "付け替え対象の子worktree名（呼び出し元と同一project内）" },
    "parent": { "type": ["string", "null"], "description": "新しい親worktree名。nullで独立化" }
  },
  "required": ["child"]
}
```

**実装概要**:
```rust
fn move_worktree(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let child = params.get("child").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("child is required"))?;
    let parent = params.get("parent").and_then(|v| v.as_str()); // null/欠落 → None

    let project_name = resolve_own_project(conn, from_session)?; // dispatchと同じ自己解決パターン

    if let Some(p) = parent {
        if config::would_create_cycle(&project_name, child, p) {  // REQ-015
            anyhow::bail!("circular parent link: {} -> {}", child, p);
        }
    }
    config::save_worktree_parent(&project_name, child, parent)?;

    Ok(json!({ "child": child, "parent": parent }))
}
```

**戻り値**: `{"child": <名前>, "parent": <名前 or null>}` 🟡

**エラーケース**:
- 循環参照（`new_parent`が`child`自身または子孫） → 同期エラー「circular parent link」（REQ-015）
- `child`が呼び出し元projectに存在しない worktree名 → 同期エラー 🟡 *現状の実装は存在チェックを`config::would_create_cycle`/`save_worktree_parent`内部のHashMapエントリ操作に委ねており、明示的な存在検証は要実装時に追加確認*

---

### spawn_child_worktree 🔵

**信頼性**: 🔵 *requirements.md REQ-022, REQ-023、既存 `finalize_add_worktree`（`main.rs:2122-2223`）より*

**説明**: 指揮者Claudeが自身配下に新規worktreeを生成し、生成直後に親リンクを設定する。既存 `finalize_add_worktree` を状態レス化した内部関数（REQ-022）をMCP経由で呼び出す。

**関連要件**: REQ-022, REQ-023

**入力スキーマ**:
```json
{
  "type": "object",
  "properties": {
    "parent": { "type": "string", "description": "生成した子の親とするworktree名（通常は呼び出し元自身）" },
    "branch": { "type": "string" },
    "worktree_name": { "type": "string", "description": "省略時は既存のworktree名自動採番規則に従う" }
  },
  "required": ["parent", "branch"]
}
```

> **実装上の留意点** 🟡: この呼び出しはTUI本体プロセス側の状態（`app.projects`, `claude_terms`）を変更する必要があるが、MCPツールは別プロセス（`siki mcp`）で動作する。既存の `send_message`/`dispatch` と異なり、単純な `messages` テーブルへのINSERTでは完結しない（worktreeの実ファイルシステム作成・`app`構造体への追加を伴うため）。既存アーキテクチャにMCPプロセスからTUI本体プロセスの状態を直接変更する経路が無いため、以下のいずれかの実装方式が必要——(a) `messages`テーブルに`message_type='spawn_request'`相当の行を置きTUI側Tickで処理する（dispatchと同じブリッジパターンを転用）、(b) 他の代替IPC。本設計では要件定義書の記述に従い(a)を推奨するが、既存コードに前例がないため実装フェーズでの詳細検証が必要 🔴。

**戻り値**: `{"worktree": <生成された名前>, "parent": <親名>}` 🟡

---

## 既存ツールの拡張

### list_sessions の scope 拡張 🔵

**信頼性**: 🔵 *requirements.md REQ-019、実地調査 `tools.rs:31-147`, `protocol.rs`の該当schema*

既存 `list_sessions` ツール（`tools.rs:31-147`）の `scope` 引数に `"children"` を追加する。

**スキーマ変更**（`protocol.rs`内、既存`enum`配列への追加）:
```diff
  "scope": {
    "type": "string",
-   "enum": ["machine", "project", "worktree"],
+   "enum": ["machine", "project", "worktree", "children"],
    "description": "Filter scope (default: machine)"
  }
```

**実装変更**（`tools.rs`内、既存`match scope`への追加アーム）:
```rust
"children" => all_sessions.iter()
    .filter(|s| {
        !proj.is_empty() && s.project_name == proj
            && config::get_descendants(proj, wt).contains(&s.worktree_name)
    })
    .collect(),
```

**戻り値**: 既存と同一形式（`sessions`配列がフィルタされるのみ）。呼び出し元自身が子を持たない場合は空配列 🔵。

---

## 関連文書

- **アーキテクチャ**: [architecture.md](architecture.md)
- **Rust型定義**: [interfaces.rs](interfaces.rs)
- **データフロー**: [dataflow.md](dataflow.md)
- **要件定義**: [requirements.md](../../spec/conductor-hierarchy/requirements.md)

## 信頼性レベルサマリー

- 🔵 青信号: 7件 (58%)
- 🟡 黄信号: 4件 (33%)
- 🔴 赤信号: 1件 (8%、`spawn_child_worktree`のプロセス間状態変更経路)

**品質評価**: 要改善（`spawn_child_worktree`のみ、MCPプロセス→TUI本体プロセスへの状態変更を伝える既存の前例が無いため、実装方式を`/tsumiki:kairo-tasks`または実装フェーズで確定する必要がある。他2ツール・拡張は高品質）

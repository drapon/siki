# エージェント監視システム データフロー図

**作成日**: 2026-06-23
**関連アーキテクチャ**: [architecture.md](architecture.md)
**関連要件定義**: [requirements.md](../../spec/agent-monitoring-system/requirements.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: EARS要件定義書・設計文書・ユーザヒアリング・既存コードを参考にした確実なフロー
- 🟡 **黄信号**: 上記資料から妥当な推測によるフロー
- 🔴 **赤信号**: 上記資料にない推測によるフロー

---

## システム全体のデータフロー 🔵

**信頼性**: 🔵 *requirements.md・コードベース調査より*

```mermaid
flowchart TD
    A[Claude Code エージェント] -->|"PreToolUse hook<br/>tool_name/tool_input"| B["siki hook-event working<br/>(hook_event.rs)"]
    B -->|format_activity| C["{event:working,<br/>session_id, activity}"]
    C -->|Unix socket| D["Broker (broker.rs)"]
    D -->|update_session_activity| E[("SQLite sessions.activity")]
    D -->|handle_event| F["SessionRegistry<br/>Session.activity"]
    D -->|AppEvent::SessionUpdate| G["TUI イベントループ"]
    G -->|再描画 (100ms Tick含む)| H["render()"]
    F -.->|ライブ参照| H
    H --> I["render_agent_popup / render_agent_dashboard"]
    I --> J[ユーザー]
```

## 主要機能のデータフロー

### 機能1: ツール活動（activity）の取得・保持 🔵

**信頼性**: 🔵 *ユーザーストーリー2.1・受け入れ基準 TC-001 より*

**関連要件**: REQ-001, REQ-002, REQ-102, REQ-103

```mermaid
sequenceDiagram
    participant CC as Claude Code
    participant HE as hook_event.rs
    participant BR as broker.rs
    participant REG as SessionRegistry
    participant DB as SQLite

    CC->>HE: PreToolUse (stdin JSON: tool_name, tool_input)
    HE->>HE: format_activity(tool_name, tool_input)
    Note over HE: Bash→description優先 / Edit→basename<br/>改行・制御文字を1行に正規化
    HE->>BR: {event:"working", session_id, activity}
    BR->>DB: update_session_state("working") + update_session_activity(activity)
    BR->>REG: handle_event(Working{session_id, activity})
    Note over REG: 未登録なら自動登録→activity をSomeで更新→state=Working
    BR->>REG: (PostToolUse/Stop 後も activity は保持: REQ-103)
```

**詳細ステップ**:
1. エージェントがツール実行直前に PreToolUse hook を発火し、`siki hook-event working` に `tool_name`/`tool_input` を含む JSON を stdin で渡す。
2. `format_activity()` が tool_name 別ルールで1行の activity を生成（改行・制御文字を除去/正規化、長文は後段表示で省略）。
3. broker が DB の `state`/`activity` を更新し、Registry の `Session.activity` を更新（working へ遷移）。
4. PostToolUse（refresh）/ Stop（idle）が来ても activity はクリアせず保持（直前の作業が見える）。

### 機能2: プロジェクト別ポップアップ表示（`m`） 🔵

**信頼性**: 🔵 *ユーザーストーリー1.1・受け入れ基準 TC-003 より*

**関連要件**: REQ-003, REQ-005, REQ-106, REQ-201

```mermaid
sequenceDiagram
    participant U as ユーザー
    participant M as main.rs (key dispatch)
    participant APP as app.rs (state)
    participant UI as ui/mod.rs
    participant REG as SessionRegistry

    U->>M: 左ペインで m 押下
    M->>M: カーソル位置→所属プロジェクト解決 (worktree行でも可: REQ-106)
    M->>APP: show_agent_popup=true, agent_popup_project_index=idx, scroll=0
    loop 表示中 (100ms Tick / SessionUpdate)
        UI->>REG: by_worktree(project, wt) を各 worktree で取得
        REG-->>UI: Vec<&Session> (activity/summary/role/state/last_seen/alert)
        UI->>UI: 行整形 (alert/waiting→赤強調, 経過時間, 省略)
        UI-->>U: ポップアップ描画 (centered_rect 60x50)
    end
    U->>M: j/k スクロール or Esc
    M->>APP: scroll 更新 / show_agent_popup=false
```

**詳細ステップ**:
1. 左ペインで `m` を押す。カーソルが worktree 行でも、その worktree が属するプロジェクトに解決（REQ-106）。
2. ポップアップ状態をオンにし、対象プロジェクトの index を保持。
3. 描画は対象プロジェクトの各 worktree について `SessionRegistry::by_worktree()` を呼び、セッション行を整形表示。表示中は他ペインのキーを横取り（REQ-201）。
4. `j`/`k` でスクロール、`Esc` で閉じる。アクティブセッションが無ければ「アクティブなエージェントなし」を表示（REQ-202）。

### 機能3: 全体ダッシュボード表示（`M`） 🔵

**信頼性**: 🔵 *ユーザーストーリー1.2・受け入れ基準 TC-004 より*

**関連要件**: REQ-004, REQ-006, EDGE-003

```mermaid
sequenceDiagram
    participant U as ユーザー
    participant M as main.rs
    participant APP as app.rs
    participant UI as ui/mod.rs
    participant REG as SessionRegistry

    U->>M: M (Shift+m) 押下
    M->>APP: show_agent_dashboard=true, scroll=0
    loop 表示中
        UI->>REG: all() 全セッション取得 (unknown も含む: EDGE-003)
        REG-->>UI: Vec<&Session>
        UI->>UI: 状態優先順ソート (priority desc, project, worktree)
        UI->>UI: alert/waiting→赤強調, 経過時間, 省略, スクロール窓適用
        UI-->>U: ダッシュボード描画 (centered_rect 80x80)
    end
    U->>M: j/k / Esc
```

**詳細ステップ**:
1. `M` で全体ダッシュボードを開く。
2. `SessionRegistry::all()` の全セッション（unknown/unknown も表示: EDGE-003）を取得。
3. 状態優先順（waiting > working > done > idle、`SessionState::priority()` 降順）→ project 名 → worktree 名 でソート（REQ-006）。
4. スクロール窓を適用して描画。`Esc` で閉じる。

## データ処理パターン

### 同期処理 🔵

**信頼性**: 🔵 *`hook_event.rs`・`broker.rs` より*

- hook の activity 抽出（`format_activity`）は同期・軽量。broker への送信も既存の直列経路。

### 非同期処理 🔵

**信頼性**: 🔵 *`broker.rs` 既存構造より*

- broker は接続ごとに `tokio::spawn`。DB 書き込み・Registry 更新・SessionUpdate 送信は既存どおり。本機能で非同期構造は変更しない。

### バッチ処理 🔵

**信頼性**: 🔵 *該当なし*

- バッチ処理は不要（イベント駆動で完結）。

## 状態管理フロー

### activity を含むセッション状態遷移 🔵

**信頼性**: 🔵 *`session.rs` の既存状態機械 + REQ-103 より*

```mermaid
stateDiagram-v2
    [*] --> Idle: register (activity=None)
    Idle --> Working: working (activity=Some 更新)
    Working --> Working: working (activity 更新)
    Working --> Waiting: waiting (activity 保持)
    Waiting --> Working: working (activity 更新)
    Working --> Done: idle/stale (activity 保持: REQ-103)
    Waiting --> Done: idle/stale (activity 保持)
    Done --> Working: working (activity 更新)
    Working --> [*]: dead (削除)
    Done --> [*]: dead / 5分無応答 (削除)
```

**ポイント**: activity が更新されるのは `working`（PreToolUse）受信時のみ。それ以外の遷移では**直前の activity を保持**（REQ-103）。`dead`（SessionEnd）や 5 分無応答でセッションごと削除されると activity も消える。

### UI 表示状態（ポップアップ/ダッシュボード） 🔵

**信頼性**: 🔵 *既存ポップアップパターンより*

```mermaid
stateDiagram-v2
    [*] --> 通常
    通常 --> ポップアップ表示: m (プロジェクト解決)
    通常 --> ダッシュボード表示: M
    ポップアップ表示 --> ポップアップ表示: j/k スクロール / Tick 再描画
    ダッシュボード表示 --> ダッシュボード表示: j/k スクロール / Tick 再描画
    ポップアップ表示 --> 通常: Esc
    ダッシュボード表示 --> 通常: Esc
```

## エラー・エッジケースのフロー 🟡

**信頼性**: 🟡 *EDGE-001/002/003・既存実装パターンからの妥当な推測*

```mermaid
flowchart TD
    A[working 受信] --> B{tool_input に想定キーあり?}
    B -->|なし| C["activity = tool_name のみ (EDGE-001)"]
    B -->|あり| D[ツール別整形]
    D --> E{改行/制御文字?}
    E -->|あり| F["1行に正規化 (EDGE-002)"]
    E -->|なし| G[そのまま]
    C --> H{表示幅超過?}
    F --> H
    G --> H
    H -->|超過| I["末尾を … で省略 (EDGE-102)"]
    H -->|収まる| J[そのまま表示]

    K[ダッシュボード描画] --> L{cwd 空の unknown?}
    L -->|はい| M["unknown/unknown として表示 (EDGE-003)"]
    L -->|いいえ| N[通常表示]
```

## hook/broker プロトコル仕様（内部「API」相当） 🔵

**信頼性**: 🔵 *`hook_event.rs` / `session.rs` HookEvent / `broker.rs` より*

HTTP API は無いが、本機能が触る唯一の境界プロトコルが broker への1行 JSON。activity 追加後の `working` メッセージ：

```json
{ "event": "working", "session_id": "abc123", "activity": "Edit: session.rs" }
```

- `activity` は **任意（Optional）**。従来形式 `{"event":"working","session_id":"abc123"}` も引き続き受理（REQ-401）。
- 他イベント（`waiting`/`refresh`/`idle`/`dead`/`register`）の形式は不変。
- broker 側 `HookEvent::Working { session_id, activity: Option<String> }` にデシリアライズ。

## 関連文書

- **アーキテクチャ**: [architecture.md](architecture.md)
- **型定義（Rust）**: [interfaces.rs](interfaces.rs)
- **DBスキーマ（SQLite）**: [database-schema.sql](database-schema.sql)
- **要件定義**: [requirements.md](../../spec/agent-monitoring-system/requirements.md)

## 信頼性レベルサマリー

- 🔵 青信号: 11件 (85%)
- 🟡 黄信号: 2件 (15%)
- 🔴 赤信号: 0件 (0%)

**品質評価**: 高品質（全フロー既存コードに接地。🟡 はエッジ整形の挙動推測のみ）

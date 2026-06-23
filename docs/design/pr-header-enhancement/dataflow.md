# PRヘッダー強化 データフロー図（軽量設計）

**作成日**: 2026-06-23
**関連アーキテクチャ**: [architecture.md](architecture.md)
**関連要件定義**: [requirements.md](../../spec/pr-header-enhancement/requirements.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 要件定義・ユーザヒアリング・既存実装を参考にした確実なフロー
- 🟡 **黄信号**: 要件・既存実装から妥当な推測によるフロー
- 🔴 **赤信号**: 根拠のない推測によるフロー

---

## 全体フロー 🔵

**信頼性**: 🔵 *既存データフロー + 本設計より*

PR 情報は「取得 → イベント → 状態更新 → 描画」の単方向で流れ、ユーザー操作（クリック）は
描画時に記録した矩形に対するヒットテストでブラウザ起動へ分岐する。

```mermaid
flowchart TD
    T1["トリガー①起動時 (main.rs:249)"]
    T2["トリガー②Claudeセッション完了後 (main.rs:702)"]
    T3["トリガー③worktree追加時 (main.rs:2161)"]
    FETCH["fetch_pr_info(wt_path)"]
    EV["AppEvent::PrInfo { worktree_id, info }"]
    UPD["handle: wt.pr = info (main.rs:1406)"]
    DRAW["render_branch_header()"]
    HIT["App.pr_link_area に Rect 記録"]
    CLICK["MouseDown(Left) ヒットテスト (main.rs:901)"]
    OPEN["open_in_browser(pr.url)"]

    T1 --> FETCH
    T2 --> FETCH
    T3 --> FETCH
    FETCH --> EV --> UPD --> DRAW --> HIT
    HIT -.次フレーム以降.-> CLICK --> OPEN
```

## フロー1: PR 情報の取得と状態判定 🔵

**信頼性**: 🔵 *requirements.md REQ-001/002/401 + 既存 fetch_pr_title より*
**関連要件**: REQ-001, REQ-002, REQ-401, REQ-403

```mermaid
sequenceDiagram
    participant TR as トリガー(3種)
    participant TK as tokio::spawn
    participant GH as gh CLI
    participant FN as classify_pr_status()
    participant TX as event_tx
    participant APP as App(メインループ)

    TR->>TK: 非同期取得を起動
    TK->>GH: gh pr view --json number,title,url,isDraft,reviewDecision,statusCheckRollup
    alt PRあり & 成功
        GH-->>TK: JSON
        TK->>FN: (isDraft, reviewDecision, statusCheckRollup)
        FN-->>TK: PrStatus（CIエラー最優先で判定）
        TK->>TX: AppEvent::PrInfo { info: Some(PrInfo) }
    else PRなし / gh失敗
        GH-->>TK: 非0 / 空
        TK->>TX: AppEvent::PrInfo { info: None }
    end
    TX->>APP: イベント配信
    APP->>APP: wt.pr = info
```

### 状態判定フローチャート（classify_pr_status） 🔵

**信頼性**: 🔵 *requirements.md REQ-002 状態判定の詳細より*

```mermaid
flowchart TD
    A[gh JSON] --> B{statusCheckRollup に失敗系あり?}
    B -->|Yes| CI[PrStatus::CiError 赤]
    B -->|No| C{reviewDecision == APPROVED?}
    C -->|Yes| AP[PrStatus::Approved 緑]
    C -->|No| D{isDraft == true?}
    D -->|Yes| DR[PrStatus::Draft グレー]
    D -->|No| RE[PrStatus::Ready イエロー]
```

> 失敗系 = CheckRun.conclusion ∈ {FAILURE, TIMED_OUT, ACTION_REQUIRED} または
> StatusContext.state ∈ {FAILURE, ERROR}。pending/running は失敗扱いせず通常判定へ（REQ-002）。🔵

## フロー2: ヘッダー描画とクリック領域記録 🔵

**信頼性**: 🔵 *既存 render() / render_branch_header (main_panel.rs:59,118) より*
**関連要件**: REQ-001, REQ-002, REQ-402, REQ-403

```mermaid
sequenceDiagram
    participant R as main_panel::render
    participant H as render_branch_header
    participant APP as App

    R->>APP: pr_link_area = None（毎フレーム初期化）
    R->>R: branch / pr を clone（借用解消）
    R->>H: render_branch_header(area, branch, pr, focused)
    H->>H: "ブランチ名 | タイトル #123" を Span 構築
    H->>H: PR部分の色 = focused ? 状態色 : DarkGray (REQ-402)
    H-->>R: PR部分の Rect（PRありの場合）
    R->>APP: pr_link_area = Some(rect)
```

- 状態色の対応（フォーカス時）: `CiError→Red` / `Approved→Green` / `Draft→DarkGray` / `Ready→Yellow` 🔵
- PR が無い worktree では PR 部分を描かず `pr_link_area = None`（REQ-403）🔵

## フロー3: クリックでブラウザ起動 🔵

**信頼性**: 🔵 *requirements.md REQ-003 + 既存マウス分岐 (main.rs:901) より*
**関連要件**: REQ-003

```mermaid
flowchart TD
    A[MouseEventKind::Down Left] --> B{hit_panel == Main\nかつ row == main.y?}
    B -->|No| Z[既存処理へ（タブ/選択など）]
    B -->|Yes| C{pr_link_area に\n column,row が含まれる?}
    C -->|No| Y[ブランチ名部分: フォーカス移動のみ]
    C -->|Yes| D{wt.pr.url あり?}
    D -->|Yes| E[open_in_browser url]
    D -->|No| Y
    E --> F[OS別コマンドを spawn 非ブロッキング]
    F -->|失敗| G[show_info で通知]
```

- 反応範囲は `pr_link_area`（PR部分のみ）に限定（REQ-003、ブランチ名クリックは非反応）🔵
- `spawn` で UI をブロックせずブラウザ起動。失敗時はパニックせず通知 🟡

## エラー・縮退時の挙動 🔵

**信頼性**: 🔵 *既存実装（None フォールバック）より*

| 状況 | 挙動 |
|------|------|
| PR が存在しない | `pr = None` → ヘッダーに PR 表示なし・`pr_link_area = None`（クリック無反応） |
| `gh` 未インストール/未認証 | 取得失敗 → `info = None`（従来と同じ縮退） |
| `statusCheckRollup` が空（CI設定なし） | 失敗系なし → approved/draft/ready の通常判定 |
| ブラウザ起動コマンド失敗 | `show_info` で通知、UI は継続 |

## 関連文書

- **アーキテクチャ**: [architecture.md](architecture.md)
- **要件定義**: [requirements.md](../../spec/pr-header-enhancement/requirements.md)

## 信頼性レベルサマリー

- 🔵 青信号: 大半（取得・状態判定・描画・クリック・縮退）
- 🟡 黄信号: 2件（ブラウザ起動の非ブロッキング/失敗通知）
- 🔴 赤信号: 0件

**品質評価**: 高品質

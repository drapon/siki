# PRヘッダー強化 要件定義書（軽量版）

**作成日**: 2026-06-23
**作業規模**: 軽量開発
**対象**: siki TUI 中央ペイン PR ヘッダー（`src/ui/main_panel.rs` 他）

## 概要

中央ペイン上部の `render_branch_header`（`src/ui/main_panel.rs:118`）は現在
`ブランチ名 | PRタイトル`（タイトルは常に黄色）を表示するだけで、PR の番号・状態
（draft / ready / approved / CI 失敗）が一目で分からず、PR ページを開くにはブラウザで
手動検索が必要。本要件では中央ペインヘッダーに **PR番号表示**・**状態別の色分け**・
**クリックでブラウザ起動** を追加し、ヘッダーだけで PR の状態把握とページ遷移を完結させる。

状態色は **CIエラー(赤) を最優先** とし、approved(緑) > draft(グレー) > ready(イエロー)
の順で判定する。

## 関連文書

- **ヒアリング記録**: [💬 interview-record.md](interview-record.md)
- **PRD**: なし（直前の計画フェーズのコード調査 + ユーザヒアリングをソースとする）

## 主要機能要件

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 設計文書・ユーザヒアリングを参考にした確実な要件
- 🟡 **黄信号**: 設計文書・既存実装から妥当な推測による要件
- 🔴 **赤信号**: 根拠のない推測による要件

### 必須機能（Must Have）

- **REQ-001**: システムはヘッダーの PR 表示に PR 番号を `タイトル #123` 形式
  （タイトルの後ろに `#番号`）で表示しなければならない 🔵 *ユーザヒアリング（番号表示要望・配置確定）*
- **REQ-002**: システムは PR 状態に応じてヘッダーの PR 表示（タイトル+番号）を色分け
  しなければならない。優先順位は **CIエラー=Red > approved=Green > draft=DarkGray >
  ready=Yellow**（CIエラーが他状態より最優先） 🔵 *ユーザヒアリング（色・優先順位確定）*
- **REQ-003**: ユーザーがヘッダーの **PR 部分（`タイトル #123` の文字範囲）** を左クリック
  した場合、システムは当該 PR ページを既定ブラウザで開かなければならない 🔵 *ユーザヒアリング（クリック要望・範囲確定）*

#### REQ-002 状態判定の詳細

- **CIエラー判定**: `statusCheckRollup` に失敗系（CheckRun の `conclusion` が
  FAILURE/TIMED_OUT/ACTION_REQUIRED、または StatusContext の `state` が FAILURE/ERROR）が
  1件でもあれば CIエラー 🔵
- **CI進行中(pending/running)**: 失敗ではないので**赤にせず**、approved/draft/ready の
  通常判定にフォールバックする 🔵 *ユーザヒアリング（pending は通常状態）*
- **approved**: `reviewDecision == "APPROVED"` 🔵
- **draft**: `isDraft == true` 🔵
- 上記いずれでもなければ **ready** 🔵

### 基本的な制約

- **REQ-401**: PR 情報の取得タイミングは既存3トリガー（①起動時 ②Claude セッション
  完了後 ③worktree 追加時）のみとし、定期ポーリングは追加しない 🔵 *ユーザヒアリング（鮮度＝既存タイミングのみ）*
- **REQ-402**: 非フォーカス時の表示は既存挙動を踏襲し、PR 部分を `DarkGray` に落とす
  （状態色はフォーカス時のみ反映） 🟡 *既存 render_branch_header 実装からの妥当な推測*
- **REQ-403**: 対象 PR が存在しない worktree ではヘッダーに PR 表示を出さない
  （既存挙動踏襲） 🔵 *既存実装（pr=None で非表示）*

## 簡易ユーザーストーリー

### ストーリー1: PR 状態の一目把握

**私は** siki を使う開発者 **として**
**中央ペインのヘッダーで PR の番号と状態（draft/ready/approved/CIエラー）を色で確認したい**
**そうすることで** ペインを切り替えずに各 worktree の PR 状況を素早く把握できる

**関連要件**: REQ-001, REQ-002

### ストーリー2: ワンクリックで PR ページへ

**私は** siki を使う開発者 **として**
**ヘッダーの PR 表示をクリックして該当 PR ページをブラウザで開きたい**
**そうすることで** PR 番号を手動検索せずレビューや CI ログにすぐアクセスできる

**関連要件**: REQ-003

## 基本的な受け入れ基準

### REQ-001 / REQ-002: 番号表示と色分け

**Given（前提条件）**: PR が紐づく worktree を選択しフォーカスしている
**When（実行条件）**: ヘッダーが描画される
**Then（期待結果）**: `ブランチ名 | タイトル #123` 形式で表示され、PR 部分が状態色になる

**テストケース**:
- [ ] 正常系: draft → グレー、ready → イエロー、approved → グリーン、CI失敗 → 赤
- [ ] 優先系: approved かつ CI失敗 → 赤（CI最優先）
- [ ] 優先系: draft かつ CI失敗 → 赤
- [ ] pending系: CI進行中のみ（失敗なし）の approved → グリーン（pendingは無視）
- [ ] 異常系: PR 無し → PR 表示を出さない

### REQ-003: クリックでブラウザ起動

**Given（前提条件）**: PR が紐づく worktree のヘッダーが表示されている
**When（実行条件）**: ユーザーが PR 部分（`タイトル #123` の範囲）を左クリックする
**Then（期待結果）**: 当該 PR の URL が既定ブラウザで開く

**テストケース**:
- [ ] 正常系: PR 部分クリック → ブラウザ起動コマンドが PR URL を引数に実行される
- [ ] 範囲外: ブランチ名部分のクリックではブラウザを開かない（PR部分のみ反応）

## 最小限の非機能要件

- **パフォーマンス**: PR 情報取得は既存同様に非同期（`tokio::spawn`）で UI をブロックしない。
  ブラウザ起動は `spawn` で非ブロッキング。🔵 *既存実装パターン踏襲*
- **互換性**: ブラウザ起動は macOS(`open`)/Linux(`xdg-open`)/Windows(`start`) を
  `cfg(target_os)` で出し分ける。🟡 *多OS対応の妥当な推測（本リポジトリは darwin 前提）*

## 参考: 実装対象（/tsumiki:kairo-design 向けメモ）

| ファイル | 変更概要 |
|----------|----------|
| `src/app.rs` | `PrStatus` enum・`PrInfo` struct 追加、`Worktree.pr_title`→`pr: Option<PrInfo>`、クリック領域記録用 `pr_link_area: Option<Rect>` 追加、初期化（`app.rs:694`/`main.rs:2150`）更新 |
| `src/main.rs` | `fetch_pr_title`→`fetch_pr_info`（`gh pr view --json number,title,url,isDraft,reviewDecision,statusCheckRollup` + 状態判定を純粋関数化）、送信3箇所（249/702/2161）・受信1箇所（1406）更新、ヘッダークリック判定（行901分岐）+ `open_in_browser` ヘルパー追加 |
| `src/event.rs` | `AppEvent::PrInfo { title }` → `{ info: Option<PrInfo> }` |
| `src/ui/main_panel.rs` | `render_branch_header` の表示（`タイトル #123`+状態色）変更、PR 部分の Rect を返し `app.pr_link_area` に記録 |

依存: `serde_json`（Cargo.toml:21 に既存）で JSON パース。状態判定ロジックは
純粋関数に切り出してユニットテスト（CIエラー最優先・pending無視・approved/draft/ready・PR無し）。

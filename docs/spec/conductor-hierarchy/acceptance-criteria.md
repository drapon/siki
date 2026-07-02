# 指揮者階層アーキテクチャ 受け入れ基準

**作成日**: 2026-07-02
**関連要件定義**: [requirements.md](requirements.md)
**関連ユーザストーリー**: [user-stories.md](user-stories.md)
**ヒアリング記録**: [interview-record.md](interview-record.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: 設計計画書・コンテキストノート・ヒアリングを参考にした確実な基準
- 🟡 **黄信号**: 上記資料から妥当な推測による基準
- 🔴 **赤信号**: 上記資料にない推測による基準

---

## REQ-001〜003: dispatch プリミティブ（送信〜配送〜既読化） 🔵

**信頼性**: 🔵 *設計計画書 Phase0-1, 0-2, 0-3*

### Given（前提条件）
- 指揮者worktreeAと子worktreeBが同一project内に存在する
- worktreeBのClaude Codeタブ（tab0）が起動済み・idle状態である

### When（実行条件）
- 指揮者AのClaudeが `dispatch({target:{type:"worktree", id:"B"}, prompt:"README.mdの行数は?"})` を呼ぶ

### Then（期待結果）
- `messages` テーブルに `message_type='dispatch', content='README.mdの行数は?'` の行がINSERTされる
- 100ms以内（次回Tick）にworktreeBのtab0 PTYへ `"README.mdの行数は?\n"` が書き込まれる
- 書き込み成功後、当該dispatchレコードが既読化（`read_at`セット）される

### テストケース

#### 正常系
- [ ] **TC-001-01**: idle状態の子へdispatch → 100ms内にPTYへ投入・既読化される 🔵
- [ ] **TC-001-02**: `sqlite3 ~/.siki/siki.db "select id,message_type,read_at from messages where message_type='dispatch'"` で既読化を確認できる 🔵 *設計計画書 Phase0検証手順*
- [ ] **TC-001-03**: 応答完了しBがidleに復帰後、再度dispatchしても正常に投入される（loop不要の実証） 🔵 *設計計画書 Phase0検証手順*

#### 異常系
- [ ] **TC-001-E01**: `target.id` に存在しないworktree名を指定 → EDGE-001に従い既読化されずアラート対象となる 🟡

---

## REQ-004: working中の子への即時投入 🔵

**信頼性**: 🔵 *ヒアリングQ1*

### Given（前提条件）
- worktreeBのClaudeがタスク実行中（working状態）である

### When（実行条件）
- 指揮者AがworktreeBへdispatchする

### Then（期待結果）
- システムはworktreeBのworking状態に関わらず、次回Tickで即座にPTYへ書き込む（待機・キューイングのロジックを持たない）

### テストケース
- [ ] **TC-004-01**: working状態の子へdispatch → 待機せず即PTYへ書き込まれる 🔵
- [ ] **TC-004-02**: working状態でも既読化タイミングはidle時と同一（書き込み成功時点）である 🔵

---

## REQ-005〜006, EDGE-101: PTY未生成時のリトライとアラート 🟡

**信頼性**: 🟡 *note.md注意事項、ヒアリングQ2*

### Given（前提条件）
- worktreeBのClaudeタブが未起動（PTY未生成）である

### When（実行条件）
- 指揮者AがworktreeBへdispatchする

### Then（期待結果）
- 既読化されず、以後のTickごとにリトライされる
- リトライ回数が上限（設計フェーズで確定、目安30回）に達した時点で既読化され、アラートが発報される
- 上限到達後の回では再試行が行われない（EDGE-101）

### テストケース

#### 正常系
- [ ] **TC-005-01**: PTY未生成の間、既読化されずリトライが継続する 🔵
- [ ] **TC-005-02**: リトライ中にタブが起動しPTYが生成された場合、次回Tickで正常に投入・既読化される 🔵

#### 境界値
- [ ] **TC-005-B01**: リトライ回数が上限ちょうどに達した回でアラートが発報され既読化される 🟡
- [ ] **TC-005-B02**: 上限到達後の次回Tickでは当該dispatchへの再試行が行われない 🟡

#### 異常系
- [ ] **TC-005-E01**: アラート発報後、`set_alert`相当の通知が人間に届く（TUI上での可視化を確認） 🟡 *通知手段の具体実装は設計フェーズで確定*

---

## REQ-007: 通常メッセージ配信からの除外 🔵

**信頼性**: 🔵 *note.md注意事項、設計計画書Phase0-1*

### Given（前提条件）
- `messages` テーブルに `message_type='dispatch'` の未読レコードが存在する

### When（実行条件）
- 別セッションが `SessionStart` フックまたは `list_sessions` を呼ぶ（通常の保留メッセージ取得経路）

### Then（期待結果）
- dispatch種別のレコードは通常の保留メッセージ一覧に含まれない（二重配信されない）

### テストケース
- [ ] **TC-007-01**: dispatch未読レコードがある状態で `list_sessions` を呼んでも、pending_messagesにdispatch内容が含まれない 🔵
- [ ] **TC-007-02**: 既存の回帰テスト（`tools.rs:621-688`, `session_start.rs:376-425`）が dispatch除外後も0失敗で通る 🔵 *設計計画書 Phase0-1「回帰テスト期待値更新が必要」*

---

## REQ-008, NFR-101: 完全自動投入と危険ツールのゲート維持 🔵

**信頼性**: 🔵 *ヒアリングQ3*

### Given（前提条件）
- 指揮者Aが子BへdispatchでプロンプトP（例: 「ファイルを削除して」）を送る

### When（実行条件）
- Pの内容がBのClaudeにとって危険ツール実行を伴う指示である

### Then（期待結果）
- dispatch自体は人間の承認なしで即座にBのPTYへ投入される
- BのClaudeが危険ツール（削除・force push等）を実行しようとした際、既存のPreToolUse hookによる承認フロー（waiting状態）が引き続き機能する

### テストケース
- [ ] **TC-008-01**: dispatch投入自体に承認ステップが介在しないことを確認 🔵
- [ ] **TC-008-02**: 危険ツール実行時にBがwaiting状態へ遷移し、人間の承認待ちになることを確認（既存hook機構の非破壊確認） 🔵

---

## REQ-010〜012, REQ-015: parentフィールドと循環ガード 🔵

**信頼性**: 🔵 *設計計画書 Phase1-1*

### Given（前提条件）
- project内にworktree A（親候補）, B（子候補）が存在する

### When（実行条件）
- MCPツール `move_worktree({child:"B", parent:"A"})` を呼ぶ

### Then（期待結果）
- `project.json` の `WorktreeMeta` にて B の `parent` が `"A"` に更新され保存される
- 再読み込み（`load_project_meta`）後も `parent` が正しく復元される

### テストケース

#### 正常系
- [ ] **TC-010-01**: `move_worktree({child:"B", parent:"A"})` → `project.json` の B.parent が "A" になる 🔵
- [ ] **TC-010-02**: config往復（save→load）後もparentが保持される（単体テスト） 🔵 *設計計画書 Phase1検証手順*

#### 異常系（循環ガード）
- [ ] **TC-015-E01**: BがAの子である状態で `move_worktree({child:"A", parent:"B"})`（循環）を呼ぶ → 拒否されエラーが返る 🔵
- [ ] **TC-015-E02**: 自分自身を親に指定する `move_worktree({child:"A", parent:"A"})` → 拒否される 🔵 *循環の特殊ケースとして妥当な推測* 🟡

---

## REQ-013〜014: TUI付け替えとツリー描画 🔵

**信頼性**: 🔵 *設計計画書 Phase1-3, 1-4*

### Given（前提条件）
- 左ペインに複数worktreeが表示されている

### When（実行条件）
- ユーザーがキー操作で子worktreeを指揮者配下へ移動する

### Then（期待結果）
- `project.json` の parent が更新される
- 左ペインが親子DFS順（親→その子孫）のツリー構造で再描画され、深さに応じたインデント・罫線が表示される

### テストケース
- [ ] **TC-013-01**: TUIキー操作で付け替え → `project.json` 更新を確認 🔵
- [ ] **TC-014-01**: 親1・子2のツリーが正しい順序（親→子1→子2）とインデントで描画される 🔵
- [ ] **TC-014-02**: 兄弟内で最後の子の罫線表現（`is_last`相当）が正しい 🟡 *left_panel.rs既存実装からの妥当な推測*

---

## REQ-016: 親削除時の子の独立化 🔵

**信頼性**: 🔵 *ヒアリングQ4*

### Given（前提条件）
- worktree A（親）が worktree B, C（子）を持つ

### When（実行条件）
- worktree A が削除される

### Then（期待結果）
- B, C の `parent` が自動的に `None` に更新される
- B, C は独立worktreeとして通常通り選択・操作できる

### テストケース
- [ ] **TC-016-01**: 親A削除後、B.parent, C.parentが両方Noneになる 🔵
- [ ] **TC-016-02**: 削除後もBへのdispatch・BのTUI操作が独立worktreeとして正常動作する 🟡

### 未規定（設計フェーズで検討）
- [ ] **EDGE-002**: 親削除処理と子worktree自体の削除が同時に競合するケースの挙動 🔴 *本要件では未規定*

---

## REQ-017〜019, REQ-021: subtree dispatchと集約 🔵

**信頼性**: 🔵 *設計計画書 Phase2-1, 2-2*

### Given（前提条件）
- 指揮者worktree Aが子B, Cを持つ（B, Cともidle）

### When（実行条件）
- 指揮者Aが `dispatch({target:{type:"subtree", id:"A"}, prompt:"レビューして"})` を呼ぶ

### Then（期待結果）
- B, C両方へdispatchレコードが作られ、両方のPTYへプロンプトが投入される
- `list_sessions({scope:"children"})` を呼ぶとB, Cのみが返る（A自身や無関係worktreeは含まれない）

### テストケース

#### 正常系
- [ ] **TC-017-01**: subtree dispatch → B, C双方のPTYに投入をキャプチャできる 🔵 *設計計画書 Phase2検証手順*
- [ ] **TC-019-01**: `list_sessions({scope:"children"})` がAの子孫（B, C）のみを返す 🔵

#### 部分失敗系
- [ ] **TC-021-01**: BのPTYが未生成の状態でsubtree dispatch → Cへは即座に投入され、Bのみリトライ対象になる 🟡 *ヒアリング対象外だが設計計画書のリトライ機構から妥当な推測*

---

## REQ-020: 状態ロールアップ表示 🔵

**信頼性**: 🔵 *設計計画書 Phase2-3*

### Given（前提条件）
- 指揮者Aの子B（idle）, C（working）が存在する

### When（実行条件）
- 左ペインが描画される

### Then（期待結果）
- Aのバッジが子孫の状態を優先度で畳み込んだ表示になる（working > idle等、既存`aggregate_state`の優先度に従う）
- B, Cいずれかにアラートがある場合、Aのバッジにもアラートが表示される（OR畳み込み）

### テストケース
- [ ] **TC-020-01**: Cがworkingになると、Aのバッジがworking相当の表示にロールアップされる 🔵 *設計計画書 Phase2検証手順*
- [ ] **TC-020-02**: BまたはCにアラートが立つと、Aのバッジにもアラート表示が伝播する 🔵

---

## REQ-022〜023: 指揮者による子生成 🔵

**信頼性**: 🔵 *設計計画書 Phase3-1*

### Given（前提条件）
- 指揮者worktree Aが存在する

### When（実行条件）
- 指揮者Aが `spawn_child_worktree({parent:"A", branch:"feature/x"})` を呼ぶ

### Then（期待結果）
- 新規worktree（例: 自動採番名）が `branch:"feature/x"` で作成される
- 生成直後、`project.json` に当該worktreeの `parent="A"` が保存される

### テストケース
- [ ] **TC-022-01**: `spawn_child_worktree` 呼び出し → 新規worktreeが作成されparentがAになっている 🔵
- [ ] **TC-022-02**: 生成された子worktreeがTUI左ペインにAの子として表示される 🔵

---

## REQ-024〜025: loop運用パターン（運用ガイド） 🟡

**信頼性**: 🟡 *設計計画書 Phase3-2は主にコードでなく運用ガイドのため、テストは定性的確認が中心*

### Given（前提条件）
- 指揮者worktreeが `/loop` で稼働している
- ワーカーworktreeはdispatchで起動済み、タスク完了

### Then（期待結果）
- 指揮者は`/loop`により反応的に子状況をポーリングし続ける（コード変更を伴わない運用パターン）
- ワーカーはタスク完了後idleに戻り、追加のloop設定は不要

### テストケース
- [ ] **TC-024-01**: 指揮者が`/loop`実行中、子の完了報告を検知して次dispatchを行う一連の流れを手動e2eで確認する 🟡
- [ ] **TC-025-01**: ワーカーがdispatch完了後、追加操作なしにidle状態に戻ることを確認する 🔵

---

## 非機能要件テスト

### NFR-001: 既存Tickサイクルの再利用 🔵

**信頼性**: 🔵 *設計計画書 Phase0-3*

- [ ] **TC-NFR-001-01**: dispatch配送処理が新規スレッド・新規タイマーを起こさず、既存`AppEvent::Tick`ハンドラ内に実装されていることをコードレビューで確認する 🔵

### NFR-002: スキーマ非変更 🔵

**信頼性**: 🔵 *note.md確定制約2*

- [ ] **TC-NFR-002-01**: マイグレーション適用後も`messages`テーブルの列構成が変更されていないことを確認する 🔵
- [ ] **TC-NFR-002-02**: 新規テーブルが追加されていないことを確認する 🔵

### NFR-201: 左ペイン表現規約の踏襲 🟡

**信頼性**: 🟡 *left_panel.rs既存実装からの妥当な推測*

- [ ] **TC-NFR-201-01**: ツリー表示のインデント・罫線が既存の非階層表示（フラットなworktree一覧）と視覚的に一貫していることを目視確認する 🟡

---

## Edgeケーステスト

### EDGE-001: 存在しない対象へのdispatch 🟡

**信頼性**: 🟡 *REQ-006からの妥当な推測*

- [ ] **TC-EDGE-001-01**: `target.id`に存在しないworktree名を指定してdispatch → 既読化されずアラート機構が発火する 🟡

### EDGE-102: cross-project階層の禁止 🔵

**信頼性**: 🔵 *設計計画書 スコープ外項*

- [ ] **TC-EDGE-102-01**: 異なるproject配下のworktreeを親子に指定する`move_worktree`呼び出しが拒否される 🔵
- [ ] **TC-EDGE-102-02**: 異なるproject配下を親に指定する`spawn_child_worktree`呼び出しが拒否される 🔵

---

## テストケースサマリー

### カテゴリ別件数

| カテゴリ | 正常系 | 異常系 | 境界値 | 合計 |
|---------|--------|--------|--------|------|
| 機能要件（Phase0〜3） | 17 | 6 | 2 | 25 |
| 非機能要件 | 4 | 0 | 0 | 4 |
| Edgeケース | 0 | 3 | 0 | 3 |
| **合計** | **21** | **9** | **2** | **32** |

### 信頼性レベル分布

- 🔵 青信号: 24件 (75%)
- 🟡 黄信号: 7件 (22%)
- 🔴 赤信号: 1件 (3%、EDGE-002)

**品質評価**: 高品質（EDGE-002のみ設計フェーズでの詳細化が必要な未規定事項として残存）

### 優先度別テストケース

- **Must Have**: 17件（Phase0基本動作、Phase1循環ガード・親削除、Phase2 subtree/集約の基本系）
- **Should Have**: 12件（リトライ・アラート、loop運用、非機能要件）
- **Could Have**: 3件（cross-project禁止等の境界確認）

---

## テスト実施計画

### Phase 1: Phase 0 dispatch基本機能テスト
- REQ-001〜009
- 優先度: Must Have
- `cargo test` 0失敗 + 手動e2e（設計計画書 Phase0検証手順）

### Phase 2: Phase 1 階層モデルテスト
- REQ-010〜016
- 優先度: Must Have
- `cargo test` 0失敗（config往復・循環ガードの単体テスト）+ 手動TUI確認

### Phase 3: Phase 2〜3 集約・子生成・非機能・Edgeケーステスト
- REQ-017〜025, NFR-001〜201, EDGE-001〜102
- 優先度: Should Have + Could Have
- 手動e2e（設計計画書 Phase2/3検証手順）+ コードレビュー

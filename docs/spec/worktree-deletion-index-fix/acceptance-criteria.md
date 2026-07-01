# worktree削除時のインデックス整合性修正 受け入れ基準

**作成日**: 2026-07-01
**関連要件定義**: [requirements.md](requirements.md)
**関連ユーザストーリー**: [user-stories.md](user-stories.md)
**ヒアリング記録**: [interview-record.md](interview-record.md)

**【信頼性レベル凡例】**:
- 🔵 **青信号**: コード調査・ユーザヒアリングを参考にした確実な基準
- 🟡 **黄信号**: コード調査・ヒアリングから妥当な推測による基準
- 🔴 **赤信号**: コード調査・ヒアリングにない推測による基準

---

## REQ-001〜REQ-003: PTY/Claudeタブの id ベース解決（Bug A） 🔵

**信頼性**: 🔵 *コード調査 (`terminal.rs`, `main.rs:41-56,716-777`) + 実環境再現より*

### Given（前提条件）
- 同一プロジェクト内に4つ以上の worktree があり、削除対象より後ろの worktree で
  Claude タブ／通常ターミナルタブを開いて PTY プロセスが稼働している

### When（実行条件）
- 削除対象より前の worktree を削除確定する（`handle_archive_confirm_key` の `y`/`Enter`）

### Then（期待結果）
- 削除対象より後ろの worktree の HashMap キーは `reindex_worktree_maps` により1つ前方へ
  シフトする
- 既に起動済みの PTY 読み取りスレッドが古い `worktree_id`/`tab_index` でイベントを送っても、
  `terminal_id` 一致による再探索でシフト後の正しいキーへ到達し、画面が更新され続ける

### テストケース

#### 正常系

- [ ] **TC-001-01**: 4 worktree 構成で先頭に近い worktree を削除 → 末尾側 worktree の
  Claude タブが出力更新を継続して受け取る 🔵
  - **入力**: `claude_terms` に `(pi,3)` キーで id=42 の `TerminalEmulator` が存在する状態で
    `(pi,2)` を削除
  - **期待結果**: `reindex_worktree_maps` 後 `claude_terms` のキーは `(pi,2)`。この状態で
    `worktree_id=(pi,3), tab_index=CLAUDE_TAB_BASE, terminal_id=42` のイベントを処理すると
    `resolve_claude_term_key` が `(pi,2)` を返す
  - **信頼性**: 🔵 *修正後コードのユニットテストで直接検証可能*

- [ ] **TC-001-02**: 同条件を通常ターミナル（`terminals`）でも検証 🔵
  - **入力**: 上記と同様の状況で `terminals` の id ベース解決ヘルパーを使用
  - **期待結果**: 新設ヘルパーが `(pi,2)` を返し `emu.process(&data)` が正しい実体に対して
    呼ばれる
  - **信頼性**: 🔵 *REQ-005 の新規実装に対する直接テスト*

#### 異常系

- [ ] **TC-001-E01**: 実際にタブが閉じられた後に届く遅延イベント 🔵
  - **入力**: `terminal_id=42` の `TerminalEmulator` が既に `claude_terms`/`terminals` から
    削除された状態で、`terminal_id=42` を含む `TerminalOutput` イベントが届く
  - **期待結果**: 再探索は失敗し `None`。イベントは黙って破棄され、パニックやエラー表示は
    発生しない（REQ-401）
  - **信頼性**: 🔵 *ヒアリングで確定した現行動作の維持*

#### 境界値

- [ ] **TC-001-B01**: 削除対象が worktree 一覧の先頭（wi=0）🔵
  - **入力**: 4 worktree のうち `wi=0` を削除
  - **期待結果**: `wi=1,2,3` はすべて1つ前方へシフトし、各々の稼働中タブが継続動作する
  - **信頼性**: 🔵 *EDGE-101 に対応*

- [ ] **TC-001-B02**: 削除対象が worktree 一覧の末尾 🔵
  - **入力**: 4 worktree のうち最終 `wi` を削除
  - **期待結果**: 他 worktree のキーはシフトされず（シフト対象が存在しない）、reindex は
    何も行わずに正常終了する
  - **信頼性**: 🔵 *EDGE-101 に対応*

---

## REQ-002 / REQ-006: ClaudeSession の id 化と内容差し替わり防止（Bug B） 🔵

**信頼性**: 🔵 *コード調査 (`app.rs:611-627`, `claude.rs`) より*

### Given（前提条件）
- `send_to_claude` 経由で worktree A の `ClaudeSession` がストリーミング中である
- 同一プロジェクト内に worktree A より前の worktree が存在する

### When（実行条件）
- worktree A より前の worktree を削除し、A の `worktree_index` がシフトする

### Then（期待結果）
- `ClaudeSession` の読み取りタスクが古い `worktree_id` でイベントを送り続けても、`id` 一致の
  再探索で `sessions` 内の現在のキーが特定され、そのキーで `app.worktree_by_id_mut` が
  呼ばれるため、シフト後にそのキーへ来た**別の実在 worktree** の `chat_history` に誤って
  書き込まれない

### テストケース

#### 正常系

- [ ] **TC-002-01**: `ClaudeOutput` イベントの `session_id` 一致でキーを再解決してから
  `chat_history` を更新する 🔵
  - **入力**: `sessions` に `session_id=7` の `ClaudeSession` が `(pi,1)` キーで存在する状態
    （worktree 削除で `(pi,2)`→`(pi,1)` にシフト済み）で、`worktree_id=(pi,2), session_id=7`
    の `ClaudeOutput` イベントを処理
  - **期待結果**: `(pi,1)` の worktree の `chat_history` が更新され、`(pi,2)`（シフト後に
    別の実在 worktree が来ている可能性がある）は更新されない
  - **信頼性**: 🔵 *REQ-006 の新規実装に対する直接テスト*

#### 異常系

- [ ] **TC-002-E01**: `session_id` 不一致（セッションが既に終了・置き換わっている）🟡
  - **入力**: 一致する `session_id` が `sessions` に存在しない
  - **期待結果**: イベントは黙って破棄される。`chat_history` は変更されない
  - **信頼性**: 🟡 *REQ-401 の準用として妥当に推測*

---

## REQ-102 / REQ-103: プロジェクト削除時の reindex（Bug C） 🔵

**信頼性**: 🔵 *コード調査 (`main.rs:4417-4483`) で確認した未実装箇所*

### Given（前提条件）
- 3つ以上のプロジェクトが登録されており、少なくとも1つは末尾以外の位置にある
- 削除対象より後ろのプロジェクトに `sessions`/`terminals`/`claude_terms` のエントリが存在する

### When（実行条件）
- 末尾以外のプロジェクトを削除確定する（`handle_remove_project_confirm_key` の `y`/`Enter`）

### Then（期待結果）
- 新設 `reindex_project_maps` により、削除位置より後ろの `project_index` を持つ全エントリの
  キーが1つ前方へシフトする
- `app.selected_worktree` が削除対象より後ろのプロジェクトを指していた場合、`project_index`
  が1つ前方へ補正される

### テストケース

#### 正常系

- [ ] **TC-101-01**: 3プロジェクト構成で中間のプロジェクトを削除 🔵
  - **入力**: `siki`(0), `api`(1), `web`(2) のうち `api`(1) を削除。`web` の worktree に
    `sessions`/`terminals`/`claude_terms` のエントリが `(2, *)` キーで存在
  - **期待結果**: 削除後 `web` は project_index=1 となり、対応するエントリはすべて `(1, *)`
    キーへシフトしている
  - **信頼性**: 🔵 *REQ-102 の新規実装に対する直接テスト*

#### 境界値

- [ ] **TC-101-B01**: プロジェクトが1つしかない状態でそれを削除 🔵
  - **入力**: プロジェクトが1件のみの状態で削除
  - **期待結果**: `reindex_project_maps` はシフト対象が存在しないため何もせず正常終了する
  - **信頼性**: 🔵 *EDGE-102 に対応*

- [ ] **TC-101-B02**: 削除対象が末尾のプロジェクト 🔵
  - **入力**: 末尾のプロジェクトを削除
  - **期待結果**: 他プロジェクトのキーはシフトされない（シフト対象が存在しない）
  - **信頼性**: 🔵 *境界値の妥当性確認*

---

## 非機能要件テスト

### NFR-001: パフォーマンス 🟡

**信頼性**: 🟡 *README記載の利用規模からの妥当な推測*

- [ ] **TC-NFR-001-01**: id 全件スキャンのコスト 🟡
  - **測定項目**: `resolve_claude_term_key`（および新設ヘルパー）1回あたりの走査コスト
  - **目標値**: 数十エントリ規模で無視できるレベル（明確な定量目標値はなし。実測での劣化が
    ないことを目視確認する）
  - **測定条件**: 1プロジェクトに worktree 5件、各 worktree に Claude タブ2件+ターミナル
    タブ5件程度の典型構成
  - **信頼性**: 🟡 *定量的な性能目標はヒアリング未実施のため推測*

---

## Edgeケーステスト

### EDGE-001: tab_index シフトと worktree_id シフトの同時発生 🟡

**信頼性**: 🟡 *既存 `resolve_claude_term_key` のコメントを踏まえた妥当な推測*

- [ ] **TC-EDGE-001-01**: 同一 worktree 内で Ctrl+W によるタブ整理と worktree 削除が
  両方絡むケース
  - **条件**: worktree A で Claude タブを2つ開き1つを Ctrl+W で閉じた直後（tab_index が
    詰められた状態）に、A より前の worktree を削除する
  - **期待結果**: 残っている A のタブの id 一致による解決が、worktree_id・tab_index 両方の
    シフトを経ても正しく機能する
  - **信頼性**: 🟡 *複合ケースのため実装後の重点確認が必要*

### EDGE-002: 削除操作の競合 🔵

**信頼性**: 🔵 *既存実装のスレッドモデルより*

- [ ] **TC-EDGE-002-01**: プロジェクト削除と worktree 削除が短時間に連続発生
  - **条件**: プロジェクト削除確定の直後に、別プロジェクトの worktree 削除を確定する
  - **期待結果**: 各削除確定処理内でのインメモリ reindex はメインスレッド上で同期的に完了
    しており、競合状態（データ不整合）は発生しない。実際の `git worktree remove` は
    `spawn_blocking` で非同期に進行するが、これはインメモリ状態と独立している
  - **信頼性**: 🔵 *既存のスレッドモデル (`main.rs:4556,4566-4576`) より*

---

## テストケースサマリー

### カテゴリ別件数

| カテゴリ | 正常系 | 異常系 | 境界値 | 合計 |
|---------|--------|--------|--------|------|
| 機能要件（Bug A） | 2 | 1 | 2 | 5 |
| 機能要件（Bug B） | 1 | 1 | 0 | 2 |
| 機能要件（Bug C） | 1 | 0 | 2 | 3 |
| 非機能要件 | 1 | 0 | 0 | 1 |
| Edgeケース | 0 | 0 | 0 | 2 |
| **合計** | 5 | 2 | 4 | 13 |

### 信頼性レベル分布

- 🔵 青信号: 10件 (77%)
- 🟡 黄信号: 3件 (23%)
- 🔴 赤信号: 0件 (0%)

**品質評価**: 高品質（Bug A は実環境再現込み、Bug B/C はコード調査で裏付け済み。定量的性能
目標のみ🟡で残存するが実装のブロッカーにはならない）

### 優先度別テストケース

- **Must Have**: 11件（TC-001-*, TC-002-*, TC-101-*）
- **Should Have**: 2件（TC-NFR-001-01, TC-EDGE-*）

---

## テスト実施計画

### Phase 1: Bug A（PTY/Claudeタブのフリーズ）修正とテスト
- REQ-001〜REQ-005
- 優先度: Must Have

### Phase 2: Bug B（ClaudeSession内容差し替わり）修正とテスト
- REQ-002, REQ-006
- 優先度: Must Have

### Phase 3: Bug C（プロジェクト削除reindex）修正とテスト
- REQ-102, REQ-103
- 優先度: Must Have

### Phase 4: Edge/非機能確認
- EDGE-001, EDGE-002, NFR-001
- 優先度: Should Have

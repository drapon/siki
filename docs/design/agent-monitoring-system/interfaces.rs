//! エージェント監視システム 型定義（設計ドキュメント）
//!
//! 作成日: 2026-06-23
//! 関連設計: architecture.md / dataflow.md
//!
//! 注意: これは設計用の参照スケッチであり、そのままビルドする前提の
//! コードではない。実装時は既存 `src/session.rs` / `src/hook_event.rs` /
//! `src/db.rs` / `src/app.rs` / `src/ui/mod.rs` の該当箇所に組み込む。
//!
//! 信頼性レベル:
//! - 🔵 青信号: EARS要件定義書・設計文書・既存コードを参考にした確実な定義
//! - 🟡 黄信号: 上記から妥当な推測による定義
//! - 🔴 赤信号: 上記にない推測による定義

// ============================================================
// 1. 状態モデル拡張（src/session.rs）
// ============================================================

/// セッション情報（既存 `Session` への activity 追加）
/// 🔵 信頼性: 既存 `session.rs:52` + ヒアリング（レベルB）より
pub struct Session {
    pub session_id: String,   // 🔵 既存
    pub worktree_name: String, // 🔵 既存
    pub project_name: String,  // 🔵 既存
    pub cwd: String,           // 🔵 既存
    pub role: String,          // 🔵 既存
    pub state: SessionState,   // 🔵 既存
    pub last_seen: std::time::Instant, // 🔵 既存（経過時間算出に使用: REQ-005）
    pub idle_pending_since: Option<std::time::Instant>, // 🔵 既存
    pub alert: bool,           // 🔵 既存（赤強調: REQ-104）
    pub alert_message: Option<String>, // 🔵 既存

    /// 現在/直前に実行中のツール活動（人間可読の1行）
    /// 🔵 REQ-001/REQ-103: working で更新、他遷移では保持
    pub activity: Option<String>,
}

/// Hook から送信されるイベント（既存 `HookEvent` の Working を拡張）
/// 🔵 信頼性: 既存 `session.rs:78` + REQ-401（後方互換）より
#[serde(tag = "event", rename_all = "lowercase")]
pub enum HookEvent {
    Register { session_id: String, cwd: String, role: String }, // 🔵 既存
    /// 🔵 activity を Optional 追加。従来形式 {"event":"working","session_id":..} も受理
    Working {
        session_id: String,
        #[serde(default)]
        activity: Option<String>, // 🔵 REQ-401: serde(default) で後方互換
    },
    Waiting { session_id: String }, // 🔵 既存（不変）
    Idle { session_id: String },    // 🔵 既存（不変）
    Dead { session_id: String },    // 🔵 既存（不変）
    Refresh { session_id: String }, // 🔵 既存（不変）
}

// SessionState（既存・変更なし）。priority() をダッシュボードのソートに流用。
// 🔵 信頼性: 既存 `session.rs:6` / `priority()`(Waiting4>Working3>Done2>Idle1)
// enum SessionState { Working, Waiting, Done, Idle }

/// SessionRegistry に追加するアクセサ（描画用）
/// 🔵 信頼性: 既存 `by_worktree` / `all` パターンに準拠
impl SessionRegistry {
    /// working 受信時に activity を設定（None のときは更新しない）
    /// 🔵 REQ-102/REQ-103
    pub fn set_activity(&mut self, session_id: &str, activity: Option<String>) {
        if let (Some(s), Some(a)) = (self.sessions.get_mut(session_id), activity) {
            s.activity = Some(a);
        }
    }

    // 既存 by_worktree(project, worktree) -> Vec<&Session> を popup で利用 🔵
    // 既存 all() -> impl Iterator<Item=&Session> を dashboard で利用 🔵
}

// ============================================================
// 2. activity 整形ヘルパ（src/hook_event.rs）
// ============================================================

/// PreToolUse payload からツール活動の1行を生成する。
/// 🔵 信頼性: ヒアリング（description優先 / ツール別ルール）+ EDGE-001/002
///
/// 入力: tool_name (&str), tool_input (&serde_json::Value)
/// 出力: 例「Bash: テスト実行」「Edit: session.rs」「Task(Explore): 調査」
///
/// ルール:
/// - Bash       → description 優先、無ければ command（🔵 ヒアリング: description優先）
/// - Edit/Write/MultiEdit/Read/NotebookEdit → file_path のベース名（🔵）
/// - Task       → "Task({subagent_type}): {description}"（🟡 妥当な推測）
/// - Grep/Glob  → "{tool}: {pattern}"（🟡 妥当な推測）
/// - その他     → tool_name のみ（🔵 EDGE-001）
///
/// 後処理: 改行・制御文字を空白に正規化しトリム（🟡 EDGE-002）。
///        長さ制限はここでは行わず、表示側で省略する（EDGE-102）。
pub fn format_activity(tool_name: &str, tool_input: &serde_json::Value) -> String {
    // 設計スケッチ（実装は match で分岐）
    unimplemented!("see rules above")
}

/// 表示用に1行へ正規化する（改行/タブ/制御文字→空白、連続空白圧縮、トリム）
/// 🟡 信頼性: EDGE-002 表示崩れ防止
fn normalize_one_line(s: &str) -> String {
    unimplemented!()
}

// ============================================================
// 3. DB アクセス（src/db.rs）
// ============================================================

/// sessions.activity を更新する（working 受信時に broker から呼ぶ）
/// 🔵 信頼性: 既存 `update_session_state` / `update_session_summary` に準拠
pub fn update_session_activity(
    conn: &rusqlite::Connection,
    session_id: &str,
    activity: &str,
) -> anyhow::Result<()> {
    // UPDATE sessions SET activity = ?1 WHERE session_id = ?2
    unimplemented!()
}

// マイグレーション（db::init 内）: 既存の冪等 ALTER パターンに追加 🔵 REQ-402
// let _ = conn.execute_batch("ALTER TABLE sessions ADD COLUMN activity TEXT;");

// ============================================================
// 4. UI 状態（src/app.rs）
// ============================================================

/// App に追加する監視ビューの状態フィールド
/// 🔵 信頼性: 既存 `show_*` ポップアップパターンに準拠
pub struct AgentMonitorState {
    /// プロジェクト別ポップアップ表示中か（REQ-003）
    pub show_agent_popup: bool,            // 🔵
    /// 対象プロジェクトの index（左ペイン projects の添字）
    pub agent_popup_project_index: usize,  // 🔵 REQ-106
    /// ポップアップのスクロール位置
    pub agent_popup_scroll: usize,         // 🔵 REQ-301

    /// 全体ダッシュボード表示中か（REQ-004）
    pub show_agent_dashboard: bool,        // 🔵
    /// ダッシュボードのスクロール位置
    pub agent_dashboard_scroll: usize,     // 🔵 REQ-301
}
// 実際は App 構造体に上記フィールドを直接追加する（別構造体化は任意）。

// ============================================================
// 5. UI 描画の入力モデル（src/ui/mod.rs）
// ============================================================

/// 監視ビュー1行の描画用ビューモデル（Registry から都度構築）
/// 🔵 信頼性: REQ-005（表示項目）+ ヒアリングより
pub struct AgentRow {
    pub project_name: String,        // 🔵 ダッシュボードのグルーピング/ソート
    pub worktree_name: String,       // 🔵
    pub role: String,                // 🔵 REQ-005
    pub state: SessionState,         // 🔵 REQ-005（バッジ色/文字は既存 badge_*）
    pub activity: Option<String>,    // 🔵 REQ-005/001（None は「-」表示）
    pub summary: Option<String>,     // 🔵 REQ-005（DB summary 由来）
    pub elapsed_secs: u64,           // 🔵 REQ-005（now - last_seen）
    pub alert: bool,                 // 🔵 REQ-104（true か state==Waiting で赤強調）
}

/// ダッシュボード用ソートキー（状態優先順）
/// 🔵 信頼性: REQ-006 + 既存 `SessionState::priority()`
/// sort_by_key: (Reverse(state.priority()), project_name, worktree_name)
fn dashboard_sort_key(row: &AgentRow) -> (std::cmp::Reverse<u8>, String, String) {
    unimplemented!()
}

/// 赤強調の判定（REQ-104）
/// 🔵 alert か waiting を要対応として赤系で描画
fn is_attention(row: &AgentRow) -> bool {
    row.alert || matches!(row.state, SessionState::Waiting)
}

// 描画関数（シグネチャ設計）:
// - fn render_agent_popup(frame, app, registry: &SessionRegistry)      // centered_rect(60,50) 🔵
// - fn render_agent_dashboard(frame, app, registry: &SessionRegistry)  // centered_rect(80,80) 🔵
//   いずれも Clear → Block(Borders::ALL) → 行リスト（スクロール窓適用）

// ============================================================
// 信頼性レベルサマリー
// ============================================================
// - 🔵 青信号: 20件 (87%)
// - 🟡 黄信号: 3件 (13%)
// - 🔴 赤信号: 0件 (0%)
//
// 品質評価: 高品質（既存型・関数シグネチャに接地。🟡 は Task/Grep 整形と
//          正規化の細部のみで、実装時に確定する）

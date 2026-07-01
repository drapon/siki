// worktree削除時のインデックス整合性修正 型定義（差分サマリ）
//
// 作成日: 2026-07-01
// 関連設計: architecture.md
// 対象言語: Rust（本プロジェクトはTypeScriptを使用しないため、既存コードと同じRust構文で記載）
//
// 信頼性レベル:
// - 🔵 青信号: 要件定義書・設計文書・既存実装を参考にした確実な定義
// - 🟡 黄信号: 要件定義書・設計文書・既存実装から妥当な推測による定義
// - 🔴 赤信号: 要件定義書・設計文書・既存実装にない推測による定義
//
// 本ファイルは差分の一覧性のためのサマリであり、実装時は既存ファイル
// (src/claude.rs, src/event.rs, src/main.rs) を直接編集する。

// ========================================
// src/claude.rs の変更
// ========================================

use std::sync::atomic::{AtomicU64, Ordering};

/// ClaudeSession のグローバル一意id採番用カウンタ
/// 🔵 REQ-002。terminal.rs::NEXT_TERMINAL_ID とは独立（design-interview.md Q2で決定）
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub struct ClaudeSession {
    id: u64,              // 🔵 REQ-002 新規フィールド。生成時点で不変の一意id
    worktree_id: WorktreeId, // 既存。reader task起動時にcaptureされる値（ヒント用途に格下げ）
    // ...既存フィールド（変更なし）
}

impl ClaudeSession {
    /// 生存期間中不変の一意id。TerminalEmulator::id() と同形のアクセサ
    /// 🔵 REQ-002
    pub fn id(&self) -> u64 {
        self.id
    }
    // spawn() 内で `NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed)` により採番し、
    // read_stdout_task へ id を追加で渡す 🔵 REQ-002
}

// ========================================
// src/event.rs の変更
// ========================================

// 既存の AppEvent enum に session_id フィールドを追加する（既存フィールドは維持）
//
// AppEvent::ClaudeOutput {
//     worktree_id: WorktreeId,
//     session_id: u64,      // 🔵 新規: REQ-006 の id 解決に使用
//     event: ClaudeStreamEvent,
// },
// AppEvent::ClaudeComplete {
//     worktree_id: WorktreeId,
//     session_id: u64,      // 🔵 新規
// },
// AppEvent::ClaudeError {
//     worktree_id: WorktreeId,
//     session_id: u64,      // 🔵 新規
//     error: String,
// },

// ========================================
// src/main.rs の変更（id解決ヘルパー）
// ========================================

/// resolve_claude_term_key（既存, main.rs:41）の変更点:
/// フォールバックの `find` から `key.0 == worktree_id` 条件を削除し、
/// claude_terms 全体を terminal_id のみで走査するようにする。
/// シグネチャ自体は変更しない。
/// 🔵 REQ-004

/// terminals 用の新設ヘルパー（resolve_claude_term_key と対になる）
/// 🔵 REQ-005: 現状 terminals には再探索処理が存在せず、直接キー引きのみだったギャップを埋める
fn resolve_terminal_key(
    terminals: &std::collections::HashMap<TerminalKey, terminal::TerminalEmulator>,
    worktree_id: app::WorktreeId,
    tab_index: usize,
    terminal_id: u64,
) -> Option<TerminalKey> {
    todo!("architecture.md 参照。まず直接キー引き、次に terminal_id のみでマップ全体を走査")
}

/// sessions 用の新設ヘルパー
/// 🔵 REQ-006: 見つかった「現在のキー」を worktree 特定に用いる
/// （イベントの古い worktree_id は worktree 特定に直接使わない）
fn resolve_session_key(
    sessions: &std::collections::HashMap<app::WorktreeId, claude::ClaudeSession>,
    worktree_id: app::WorktreeId,
    session_id: u64,
) -> Option<app::WorktreeId> {
    todo!("architecture.md 参照。まず直接キー引き、次に session_id のみでマップ全体を走査")
}

// ========================================
// src/main.rs の変更（reindex_project_maps 新設）
// ========================================

/// プロジェクト削除後、削除位置より後ろの project_index を持つ
/// sessions / terminals / claude_terms のキーを1つ前にずらす。
/// reindex_worktree_maps（既存, main.rs:4592）と対になる、project_index シフト専用の新設関数。
/// 🔵 REQ-102, REQ-103
fn reindex_project_maps(
    sessions: &mut std::collections::HashMap<app::WorktreeId, claude::ClaudeSession>,
    terminals: &mut std::collections::HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut std::collections::HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    removed_project_index: usize,
) {
    todo!("architecture.md 参照。project_index > removed_project_index のキーを (pi-1, wi) へ移動")
}

// ========================================
// 信頼性レベルサマリー
// ========================================
// - 🔵 青信号: 7件 (100%)
// - 🟡 黄信号: 0件 (0%)
// - 🔴 赤信号: 0件 (0%)
//
// 品質評価: 高品質（すべての差分がrequirements.mdの要件IDに紐づく）

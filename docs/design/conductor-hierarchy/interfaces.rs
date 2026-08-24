//! 指揮者階層アーキテクチャ 型定義
//!
//! 作成日: 2026-07-02
//! 関連設計: architecture.md
//!
//! 信頼性レベル:
//! - 🔵 青信号: 要件定義書・実地コード調査(Exploreエージェント)を参考にした確実な型定義
//! - 🟡 黄信号: 要件定義書・実地コード調査から妥当な推測による型定義
//! - 🔴 赤信号: 上記にない推測による型定義
//!
//! 本ファイルは配置先ファイルの断片をまとめた設計時点の参照ドキュメントであり、
//! そのままコンパイルするcrateではない。各型・関数の配置先は末尾のコメントを参照。

// ========================================
// db.rs へ追加
// ========================================

/// dispatch専用の取得行（REQ-002）
/// 🔵 既存 MessageRow (db.rs:364-372) と同じ命名規則に揃えた新規struct
#[derive(Debug, Clone)]
pub struct DispatchRow {
    pub id: i64,               // 🔵 messages.id
    pub to_worktree: Option<String>, // 🔵 messages.to_worktree（dispatchでは常にSomeのはずだが、防御的にOptionのまま扱う）
    pub to_project: Option<String>,  // 🔵 messages.to_project（dispatch挿入時に必ずセットする、REQ-009/EDGE-102）
    pub content: String,        // 🔵 messages.content = プロンプト本文
}

// 配置先: src/db.rs
// 追加関数: pub fn get_pending_dispatches(conn: &Connection) -> Result<Vec<DispatchRow>>
// 既読化は既存 pub fn mark_messages_read(conn: &Connection, ids: &[i64]) -> Result<usize> を再利用（新規関数不要）

// ========================================
// config.rs へ追加・変更
// ========================================

/// worktree メタデータ（既存 struct への1フィールド追加）
/// 🔵 既存 config.rs:341-346 の #[serde(default, skip_serializing_if = "Option::is_none")] パターンを踏襲
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WorktreeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>, // 既存フィールド
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,        // 🔵 REQ-010: 同一project内の親worktree名。Noneは独立worktree
}

// 配置先: src/config.rs
//
// 追加関数シグネチャ:
//   pub fn save_worktree_parent(project_name: &str, worktree_name: &str, parent: Option<&str>) -> Result<()>
//     🟡 既存 save_worktree_display_name と同型のread-modify-write（実装時に既存関数の正確なパターンを踏襲すること）
//
//   pub fn get_descendants(project_name: &str, root_worktree: &str) -> Vec<String>
//     🔵 REQ-018: parentリンクを逆辿りしたDFS。空Vecは「子孫なし」
//
//   pub fn would_create_cycle(project_name: &str, child: &str, new_parent: &str) -> bool
//     🔵 REQ-015: new_parentがchild自身、またはchildの子孫ならtrue（付け替え拒否）

// ========================================
// app.rs へ追加・変更
// ========================================

// Worktree構造体（app.rs:226-255）への追加フィールド
// 🔵 design-interview.md調査より、既存 display_name と同じ扱い（Project::from_config と
//    finalize_add_worktree/spawn_child_worktree の両方で初期化が必要）
//
// pub struct Worktree {
//     // ...既存フィールド...
//     pub parent: Option<String>,  // 🔵 REQ-010〜014
// }

/// (project_name, worktree_name) → 現在のWorktreeId解決（新規実装、design-interview.md訂正2）
/// 🔵 WorktreeIdはadd/remove時に再インデックスされ不安定なため、結果をキャッシュしてはならない
// impl App {
//     pub fn find_worktree_id(&self, project_name: &str, worktree_name: &str) -> Option<WorktreeId>
// }

// ========================================
// terminal.rs へ追加
// ========================================

// impl TerminalEmulator {
//     /// PTYプロセスが生存しているか（design-interview.md訂正5: write()は非aliveでもOk(())を返すため必須）
//     pub fn is_alive(&self) -> bool { self.alive }
// }

// ========================================
// ui/left_panel.rs へ変更
// ========================================

/// フラット化リストの各行の種類（既存enumへのフィールド追加、既存2バリアント構造は維持）
/// 🔵 REQ-014: ツリー描画のための深さ情報を追加
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    Project { index: usize },
    Worktree {
        project_index: usize,
        worktree_index: usize,
        depth: usize, // 🔵 REQ-014: 0=ルート(親なし)、1以上=ネスト深さ
    },
}

// ========================================
// mcp/tools.rs 内部で使用する中間型（永続化はしない）
// ========================================

/// dispatch MCPツールのtargetパース結果（呼び出し内のローカル型、🟡 実装補助）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchTargetType {
    Worktree, // target.type == "worktree"
    Subtree,  // target.type == "subtree"（REQ-017、指定worktreeの子孫全員へfan-out）
}

// ========================================
// 信頼性レベルサマリー
// ========================================
// - 🔵 青信号: 8件 (73%)
// - 🟡 黄信号: 3件 (27%)
// - 🔴 赤信号: 0件
//
// 品質評価: 高品質

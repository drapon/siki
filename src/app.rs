use crate::config::{self, Config, ProjectConfig};
use crate::event::ClaudeStreamEvent;
use crate::selection::TextSelection;
use chrono::{DateTime, Utc};
use ratatui::prelude::Rect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Worktree の識別子 (project_index, worktree_index)
pub type WorktreeId = (usize, usize);

/// パネルフォーカス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Left,
    Main,
    Right,
    Terminal,
}

/// Worktree のステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeStatus {
    Idle,
    Running,
    Done,
}

/// 右パネル上部のモード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanelMode {
    Tree,
    Diff,
}

/// Changes モード内の上下パネルフォーカス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFocus {
    /// PR 差分ファイル一覧（base...HEAD）
    PrDiff,
    /// ローカル未コミット変更一覧（git diff HEAD）
    LocalChanges,
}

/// チャットメッセージのロール
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// チャットメッセージ
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// 開いているファイル（diff_content が Some ならdiffタブ）
#[derive(Debug, Clone)]
pub struct OpenFile {
    pub path: PathBuf,
    pub content: String,
    pub scroll_offset: usize,
    /// カーソルが置かれている行（0-indexed）
    pub cursor_line: usize,
    /// ハイライト済みスパンのキャッシュ (行ごとに [(R,G,B,text), ...])
    pub highlighted: Vec<Vec<(u8, u8, u8, String)>>,
    /// 検索モード中か
    pub search_active: bool,
    /// 検索文字列
    pub search_query: String,
    /// マッチした行番号のリスト (0-indexed)
    pub search_matches: Vec<usize>,
    /// 現在フォーカス中のマッチインデックス
    pub search_match_idx: usize,
    /// diff タブの場合、差分内容を保持（Some ならdiff表示モード）
    pub diff_content: Option<String>,
}

impl OpenFile {
    pub fn search_start(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
    }

    pub fn search_cancel(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
    }

    pub fn search_confirm(&mut self) {
        self.search_active = false;
        // マッチ結果は保持 (n/N で巡回可能)
    }

    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.update_search_matches();
    }

    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.update_search_matches();
    }

    fn update_search_matches(&mut self) {
        self.search_matches.clear();
        self.search_match_idx = 0;
        if self.search_query.is_empty() {
            return;
        }
        let query_lower = self.search_query.to_lowercase();
        for (i, line) in self.content.lines().enumerate() {
            if line.to_lowercase().contains(&query_lower) {
                self.search_matches.push(i);
            }
        }
        if let Some(&first) = self.search_matches.first() {
            self.cursor_line = first;
        }
    }

    pub fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.cursor_line = self.search_matches[self.search_match_idx];
    }

    pub fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        if self.search_match_idx == 0 {
            self.search_match_idx = self.search_matches.len() - 1;
        } else {
            self.search_match_idx -= 1;
        }
        self.cursor_line = self.search_matches[self.search_match_idx];
    }
}

/// grep 検索結果
#[derive(Debug, Clone)]
pub struct GrepResult {
    pub path: PathBuf,
    pub line_number: usize,
    pub line_content: String,
}

/// ステータスメッセージのレベル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Error,
}

/// ステータスメッセージ
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub level: StatusLevel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextAddMode {
    #[default]
    Text,
    Url,
}

/// Worktree 追加時のブランチ作成モード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddWorktreeMode {
    /// 現在の HEAD から新規ブランチを作成
    NewBranch,
    /// ベースブランチ（origin/main 等）から新規ブランチを作成
    FromBase,
    /// 既存リモートブランチをチェックアウト
    FromRemote,
}

impl AddWorktreeMode {
    pub fn next(self) -> Self {
        match self {
            Self::NewBranch => Self::FromBase,
            Self::FromBase => Self::FromRemote,
            Self::FromRemote => Self::NewBranch,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NewBranch => "新規",
            Self::FromBase => "ベース",
            Self::FromRemote => "リモート",
        }
    }
}

/// Worktree の状態
#[derive(Debug)]
pub struct Worktree {
    pub name: String,
    pub display_name: Option<String>,
    pub branch: String,
    pub path: PathBuf,
    pub status: WorktreeStatus,
    pub chat_history: Vec<ChatMessage>,
    pub open_files: Vec<OpenFile>,
    pub active_tab: usize,
    /// 起動中の Claude Code タブ数（タブ 0..claude_tabs-1 が Claude）
    pub claude_tabs: usize,
    pub right_panel_mode: RightPanelMode,
    /// Changes モード内のフォーカス位置
    pub diff_focus: DiffFocus,
    pub active_terminal: usize,
    pub chat_scroll_offset: usize,
    /// Claude ターミナルのスクロールバックオフセット（タブごと）
    pub claude_scroll_offsets: HashMap<usize, usize>,
    /// GitHub PR タイトル（ブランチに紐づく PR がある場合）
    pub pr_title: Option<String>,
    /// Claude Code のセッション ID（`-r` による再開用）
    pub claude_session_id: Option<String>,
}

/// プロジェクトの状態
#[derive(Debug)]
pub struct Project {
    pub name: String,
    pub display_name: Option<String>,
    pub path: PathBuf,
    pub worktrees: Vec<Worktree>,
    pub collapsed: bool,
}

/// アプリケーション全体の状態
#[derive(Debug)]
pub struct App {
    pub projects: Vec<Project>,
    pub selected_worktree: Option<WorktreeId>,
    pub focused_panel: Panel,
    pub status_message: Option<StatusMessage>,
    pub status_set_at: Option<Instant>,
    pub show_help: bool,
    pub help_scroll: usize,
    pub show_message_popup: bool,
    pub popup_input: String,
    pub show_add_worktree_popup: bool,
    pub add_worktree_input: String,
    pub add_worktree_project_index: usize,
    pub add_worktree_name: String,
    pub add_worktree_mode: AddWorktreeMode,
    pub add_worktree_remote_branches: Vec<String>,
    pub add_worktree_branch_filter: String,
    pub add_worktree_branch_cursor: usize,
    pub add_worktree_loading: bool,
    pub add_worktree_base_branch: String,
    pub show_add_project_popup: bool,
    pub add_project_input: String,
    pub show_grep_popup: bool,
    pub grep_input: String,
    pub grep_results: Vec<GrepResult>,
    pub grep_cursor: usize,
    pub show_archive_confirm: bool,
    pub archive_target: Option<WorktreeId>,
    pub show_remove_project_confirm: bool,
    pub remove_project_target: Option<usize>,
    pub show_siki_json_confirm: bool,
    pub siki_json_confirm_project_path: Option<std::path::PathBuf>,
    /// オーバーレイターミナル（siki.json 作成用）表示中フラグ
    pub show_siki_json_init_terminal: bool,
    /// オーバーレイターミナルのスクロールバックオフセット
    pub siki_json_init_scroll: usize,
    /// オーバーレイターミナルのスピナーカウンタ
    pub siki_json_init_spinner: usize,
    /// スキル名入力ポップアップ
    pub show_skill_name_popup: bool,
    /// スキル名入力バッファ
    pub skill_name_input: String,
    /// スキル作成対象のプロジェクト名
    pub skill_project_name: Option<String>,
    /// スキル内容入力ポップアップ表示フラグ
    pub show_skill_edit_popup: bool,
    /// スキル内容入力バッファ
    pub skill_content_input: String,
    /// スキル内容入力のカーソル位置（バイトオフセット）
    pub skill_content_cursor: usize,
    /// スキル内容を Claude で整形中
    pub skill_refining: bool,
    /// スキル整形中のスピナーカウンタ
    pub skill_refine_spinner: usize,
    /// スキル一覧ポップアップ表示フラグ
    pub show_skill_list: bool,
    /// スキル一覧データ (name, content)
    pub skill_list_items: Vec<(String, String)>,
    /// スキル一覧カーソル位置
    pub skill_list_cursor: usize,
    /// シンボリックリンク設定ポップアップ表示フラグ
    pub show_symlink_settings: bool,
    /// シンボリックリンク設定対象のプロジェクト名
    pub symlink_project_name: Option<String>,
    /// シンボリックリンク候補リスト (dir_name, is_enabled)
    pub symlink_items: Vec<(String, bool)>,
    /// シンボリックリンク設定カーソル位置
    pub symlink_cursor: usize,
    /// シンボリックリンク手動入力モード
    pub symlink_input_mode: bool,
    /// シンボリックリンク手動入力バッファ
    pub symlink_input: String,
    /// コンテキスト一覧ポップアップ表示フラグ
    pub show_context_list: bool,
    /// コンテキスト一覧データ (name, content)
    pub context_list_items: Vec<(String, String)>,
    /// コンテキスト一覧カーソル位置
    pub context_list_cursor: usize,
    /// コンテキスト対象のプロジェクト名
    pub context_project_name: Option<String>,
    /// コンテキスト対象の worktree 名
    pub context_worktree_name: Option<String>,
    /// コンテキスト名入力ポップアップ
    pub show_context_name_popup: bool,
    /// コンテキスト名入力バッファ
    pub context_name_input: String,
    /// コンテキスト追加モード (Text or Url)
    pub context_add_mode: ContextAddMode,
    /// コンテキスト編集ポップアップ表示フラグ
    pub show_context_edit_popup: bool,
    /// コンテキスト内容入力バッファ
    pub context_content_input: String,
    /// コンテキスト内容入力のカーソル位置（バイトオフセット）
    pub context_content_cursor: usize,
    /// コンテキスト編集のスクロールオフセット（行数）
    pub context_edit_scroll: usize,
    /// コンテキスト編集の選択範囲 (anchor, cursor) バイトオフセット
    pub context_edit_selection: Option<(usize, usize)>,
    /// コンテキスト編集でマウスボタンが押されている状態
    pub context_edit_dragging: bool,
    /// コンテキスト内容を Claude で整形中
    pub context_refining: bool,
    /// コンテキスト整形中のスピナーカウンタ
    pub context_refine_spinner: usize,
    /// コンテキスト URL 入力ポップアップ
    pub show_context_url_popup: bool,
    /// コンテキスト URL 入力バッファ
    pub context_url_input: String,
    /// URL からコンテンツ取得中
    pub context_url_fetching: bool,
    /// URL 取得中のスピナーカウンタ
    pub context_url_spinner: usize,
    /// セッション選択ポップアップ（新規 Claude タブ起動時）
    pub show_session_choice: bool,
    /// セッション選択対象の worktree ID
    pub session_choice_wt_id: Option<WorktreeId>,
    /// プロジェクト表示名変更ポップアップ
    pub show_rename_project_popup: bool,
    pub rename_project_input: String,
    pub rename_project_name: Option<String>,
    /// worktree リネーム対象 (project_index, worktree_index)
    pub rename_worktree_target: Option<(usize, usize)>,
    /// テキスト選択状態（Claude / ターミナルペインのマウスドラッグ選択）
    pub text_selection: Option<TextSelection>,
    /// Claude ペインのコンテンツ領域（レンダリング時に計算）
    pub claude_content_area: Option<Rect>,
    /// ターミナルペインのコンテンツ領域（レンダリング時に計算）
    pub terminal_content_area: Option<Rect>,
    /// ファイルビューのコンテンツ領域（レンダリング時に計算）
    pub file_content_area: Option<Rect>,
    /// grep 検索結果を中央ペインに表示中か
    pub show_grep_results_view: bool,
    /// grep 結果ビューのカーソル位置
    pub grep_view_cursor: usize,
    /// grep 結果ビューのスクロールオフセット
    pub grep_view_scroll: usize,
    pub running: bool,
}

impl App {
    /// Config から App の初期状態を構築する
    pub fn new(config: &Config) -> Self {
        let projects = config
            .projects
            .iter()
            .map(|pc| Project::from_config(pc))
            .collect();

        Self {
            projects,
            selected_worktree: None,
            focused_panel: Panel::Left,
            status_message: None,
            status_set_at: None,
            show_help: false,
            help_scroll: 0,
            show_message_popup: false,
            popup_input: String::new(),
            show_add_worktree_popup: false,
            add_worktree_input: String::new(),
            add_worktree_project_index: 0,
            add_worktree_name: String::new(),
            add_worktree_mode: AddWorktreeMode::NewBranch,
            add_worktree_remote_branches: Vec::new(),
            add_worktree_branch_filter: String::new(),
            add_worktree_branch_cursor: 0,
            add_worktree_loading: false,
            add_worktree_base_branch: "origin/main".to_string(),
            show_add_project_popup: false,
            add_project_input: String::new(),
            show_grep_popup: false,
            grep_input: String::new(),
            grep_results: Vec::new(),
            grep_cursor: 0,
            show_archive_confirm: false,
            archive_target: None,
            show_remove_project_confirm: false,
            remove_project_target: None,
            show_siki_json_confirm: false,
            siki_json_confirm_project_path: None,
            show_siki_json_init_terminal: false,
            siki_json_init_scroll: 0,
            siki_json_init_spinner: 0,
            show_skill_name_popup: false,
            skill_name_input: String::new(),
            skill_project_name: None,
            show_skill_edit_popup: false,
            skill_content_input: String::new(),
            skill_content_cursor: 0,
            skill_refining: false,
            skill_refine_spinner: 0,
            show_skill_list: false,
            skill_list_items: Vec::new(),
            skill_list_cursor: 0,
            show_symlink_settings: false,
            symlink_project_name: None,
            symlink_items: Vec::new(),
            symlink_cursor: 0,
            symlink_input_mode: false,
            symlink_input: String::new(),
            show_context_list: false,
            context_list_items: Vec::new(),
            context_list_cursor: 0,
            context_project_name: None,
            context_worktree_name: None,
            show_context_name_popup: false,
            context_name_input: String::new(),
            context_add_mode: ContextAddMode::default(),
            show_context_edit_popup: false,
            context_content_input: String::new(),
            context_content_cursor: 0,
            context_edit_scroll: 0,
            context_edit_selection: None,
            context_edit_dragging: false,
            context_refining: false,
            context_refine_spinner: 0,
            show_context_url_popup: false,
            context_url_input: String::new(),
            context_url_fetching: false,
            context_url_spinner: 0,
            show_session_choice: false,
            session_choice_wt_id: None,
            show_rename_project_popup: false,
            rename_project_input: String::new(),
            rename_project_name: None,
            rename_worktree_target: None,
            text_selection: None,
            claude_content_area: None,
            terminal_content_area: None,
            file_content_area: None,
            show_grep_results_view: false,
            grep_view_cursor: 0,
            grep_view_scroll: 0,
            running: true,
        }
    }

    /// フォーカスを次のパネルに切り替える
    ///
    /// Left → Main → Right → Terminal → Left の順で循環する。
    pub fn cycle_focus(&mut self, reverse: bool) {
        self.focused_panel = if reverse {
            match self.focused_panel {
                Panel::Left => Panel::Terminal,
                Panel::Main => Panel::Left,
                Panel::Right => Panel::Main,
                Panel::Terminal => Panel::Right,
            }
        } else {
            match self.focused_panel {
                Panel::Left => Panel::Main,
                Panel::Main => Panel::Right,
                Panel::Right => Panel::Terminal,
                Panel::Terminal => Panel::Left,
            }
        };
    }

    /// 選択中の worktree への参照を取得
    pub fn selected_worktree(&self) -> Option<&Worktree> {
        let (pi, wi) = self.selected_worktree?;
        self.projects.get(pi)?.worktrees.get(wi)
    }

    /// 選択中の worktree への可変参照を取得
    pub fn selected_worktree_mut(&mut self) -> Option<&mut Worktree> {
        let (pi, wi) = self.selected_worktree?;
        self.projects.get_mut(pi)?.worktrees.get_mut(wi)
    }

    /// ステータスメッセージの自動クリア時間
    const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

    /// ステータスバーにエラーメッセージを表示
    pub fn show_error(&mut self, message: String) {
        self.status_message = Some(StatusMessage {
            text: message,
            level: StatusLevel::Error,
        });
        self.status_set_at = Some(Instant::now());
    }

    /// ステータスバーに情報メッセージを表示
    pub fn show_info(&mut self, message: String) {
        self.status_message = Some(StatusMessage {
            text: message,
            level: StatusLevel::Info,
        });
        self.status_set_at = Some(Instant::now());
    }

    /// タイムアウト経過済みのステータスメッセージをクリアする
    ///
    /// Tick イベントから呼ばれ、設定時刻から STATUS_TIMEOUT 経過していれば消去する。
    pub fn clear_expired_status(&mut self) {
        if let Some(set_at) = self.status_set_at {
            if set_at.elapsed() >= Self::STATUS_TIMEOUT {
                self.status_message = None;
                self.status_set_at = None;
            }
        }
    }

    /// WorktreeId で worktree を取得する
    pub fn worktree_by_id(&self, id: WorktreeId) -> Option<&Worktree> {
        let (pi, wi) = id;
        self.projects.get(pi)?.worktrees.get(wi)
    }

    /// WorktreeId で worktree の可変参照を取得する
    pub fn worktree_by_id_mut(&mut self, id: WorktreeId) -> Option<&mut Worktree> {
        let (pi, wi) = id;
        self.projects.get_mut(pi)?.worktrees.get_mut(wi)
    }

    /// Claude Code のストリームイベントを処理する
    pub fn handle_claude_output(&mut self, worktree_id: WorktreeId, event: ClaudeStreamEvent) {
        let Some(wt) = self.worktree_by_id_mut(worktree_id) else {
            return;
        };

        match event {
            ClaudeStreamEvent::Init { session_id } => {
                // セッション初期化完了。session_id を保存（-r による再開用）
                if !session_id.is_empty() {
                    wt.claude_session_id = Some(session_id);
                }
            }
            ClaudeStreamEvent::ContentDelta { text } => {
                // 最後のメッセージが Assistant なら追記、なければ新規作成
                if let Some(last) = wt.chat_history.last_mut() {
                    if last.role == Role::Assistant {
                        last.content.push_str(&text);
                        wt.chat_scroll_offset = usize::MAX;
                        return;
                    }
                }
                wt.chat_history.push(ChatMessage {
                    role: Role::Assistant,
                    content: text,
                    timestamp: Utc::now(),
                });
                wt.chat_scroll_offset = usize::MAX;
            }
            ClaudeStreamEvent::ToolUse { tool, .. } => {
                // ツール使用をチャットに表示
                if let Some(last) = wt.chat_history.last_mut() {
                    if last.role == Role::Assistant {
                        last.content.push_str(&format!("\n[Tool: {}]", tool));
                        wt.chat_scroll_offset = usize::MAX;
                        return;
                    }
                }
                wt.chat_history.push(ChatMessage {
                    role: Role::Assistant,
                    content: format!("[Tool: {}]", tool),
                    timestamp: Utc::now(),
                });
                wt.chat_scroll_offset = usize::MAX;
            }
            ClaudeStreamEvent::Result { text } => {
                // 最終結果。内容があれば表示
                if !text.is_empty() {
                    if let Some(last) = wt.chat_history.last_mut() {
                        if last.role == Role::Assistant && last.content.is_empty() {
                            last.content = text;
                            wt.chat_scroll_offset = usize::MAX;
                            return;
                        }
                    }
                }
            }
            ClaudeStreamEvent::Error { .. } => {
                // ClaudeError イベント経由で処理される
            }
        }
    }

    /// Claude Code の応答完了を処理する
    pub fn handle_claude_complete(&mut self, worktree_id: WorktreeId) {
        if let Some(wt) = self.worktree_by_id_mut(worktree_id) {
            wt.status = WorktreeStatus::Idle;
        }
    }

    /// Claude Code のエラーを処理する
    pub fn handle_claude_error(&mut self, worktree_id: WorktreeId, error: &str) {
        if let Some(wt) = self.worktree_by_id_mut(worktree_id) {
            wt.status = WorktreeStatus::Idle;
        }
        self.show_error(format!("Claude Code エラー: {}", error));
    }
}

impl Project {
    fn from_config(pc: &ProjectConfig) -> Self {
        let worktrees = pc
            .worktrees
            .iter()
            .map(|wc| {
                let display_name = config::load_worktree_meta(&pc.name, &wc.name)
                    .and_then(|m| m.display_name);
                Worktree {
                name: wc.name.clone(),
                display_name,
                branch: wc.branch.clone(),
                path: config::worktree_path(&pc.name, &wc.name),
                status: WorktreeStatus::Idle,
                chat_history: Vec::new(),
                open_files: Vec::new(),
                active_tab: 0,
                claude_tabs: 0,
                right_panel_mode: RightPanelMode::Tree,
                diff_focus: DiffFocus::PrDiff,
                active_terminal: 0,
                chat_scroll_offset: 0,
                claude_scroll_offsets: HashMap::new(),
                pr_title: None,
                claude_session_id: None,
            }})
            .collect();

        Self {
            name: pc.name.clone(),
            display_name: pc.display_name.clone(),
            path: PathBuf::from(&pc.path),
            worktrees,
            collapsed: false,
        }
    }
}

impl WorktreeStatus {
    /// ステータスアイコンを返す
    pub fn icon(&self) -> &'static str {
        match self {
            WorktreeStatus::Running => "●",
            WorktreeStatus::Idle => "○",
            WorktreeStatus::Done => "✓",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SikiConfig, WorktreeConfig};

    fn sample_config() -> Config {
        Config {
            siki: SikiConfig {
                shell: Some("/bin/zsh".to_string()),
                shared_dirs: vec!["node_modules".to_string()],
                base_branch: None,
            },
            projects: vec![
                ProjectConfig {
                    name: "webapp".to_string(),
                    path: "/home/user/webapp".to_string(),
                    display_name: None,
                    worktrees: vec![
                        WorktreeConfig {
                            name: "feature-auth".to_string(),
                            branch: "feature/auth".to_string(),
                        },
                        WorktreeConfig {
                            name: "fix-bug".to_string(),
                            branch: "fix/bug-123".to_string(),
                        },
                    ],
                },
                ProjectConfig {
                    name: "api".to_string(),
                    path: "/home/user/api".to_string(),
                    display_name: None,
                    worktrees: vec![WorktreeConfig {
                        name: "refactor".to_string(),
                        branch: "refactor/db".to_string(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn test_app_new_from_config() {
        let config = sample_config();
        let app = App::new(&config);

        assert_eq!(app.projects.len(), 2);
        assert_eq!(app.projects[0].name, "webapp");
        assert_eq!(app.projects[0].worktrees.len(), 2);
        assert_eq!(app.projects[1].name, "api");
        assert_eq!(app.projects[1].worktrees.len(), 1);
        assert!(app.selected_worktree.is_none());
        assert_eq!(app.focused_panel, Panel::Left);
        assert!(app.running);
        assert!(!app.show_help);
        assert!(!app.show_message_popup);
    }

    #[test]
    fn test_worktree_path_construction() {
        let config = sample_config();
        let app = App::new(&config);

        let wt = &app.projects[0].worktrees[0];
        assert_eq!(
            wt.path,
            crate::config::worktree_path("webapp", "feature-auth")
        );
    }

    #[test]
    fn test_worktree_initial_state() {
        let config = sample_config();
        let app = App::new(&config);

        let wt = &app.projects[0].worktrees[0];
        assert_eq!(wt.status, WorktreeStatus::Idle);
        assert!(wt.chat_history.is_empty());
        assert!(wt.open_files.is_empty());
        assert_eq!(wt.active_tab, 0);
        assert_eq!(wt.right_panel_mode, RightPanelMode::Tree);
    }

    #[test]
    fn test_project_not_collapsed_by_default() {
        let config = sample_config();
        let app = App::new(&config);

        assert!(!app.projects[0].collapsed);
        assert!(!app.projects[1].collapsed);
    }

    #[test]
    fn test_cycle_focus_forward() {
        let config = sample_config();
        let mut app = App::new(&config);

        assert_eq!(app.focused_panel, Panel::Left);
        app.cycle_focus(false);
        assert_eq!(app.focused_panel, Panel::Main);
        app.cycle_focus(false);
        assert_eq!(app.focused_panel, Panel::Right);
        app.cycle_focus(false);
        assert_eq!(app.focused_panel, Panel::Terminal);
        app.cycle_focus(false);
        assert_eq!(app.focused_panel, Panel::Left);
    }

    #[test]
    fn test_cycle_focus_reverse() {
        let config = sample_config();
        let mut app = App::new(&config);

        assert_eq!(app.focused_panel, Panel::Left);
        app.cycle_focus(true);
        assert_eq!(app.focused_panel, Panel::Terminal);
        app.cycle_focus(true);
        assert_eq!(app.focused_panel, Panel::Right);
        app.cycle_focus(true);
        assert_eq!(app.focused_panel, Panel::Main);
        app.cycle_focus(true);
        assert_eq!(app.focused_panel, Panel::Left);
    }

    #[test]
    fn test_selected_worktree_none() {
        let config = sample_config();
        let app = App::new(&config);

        assert!(app.selected_worktree().is_none());
    }

    #[test]
    fn test_selected_worktree_some() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.selected_worktree = Some((0, 1));
        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.name, "fix-bug");
        assert_eq!(wt.branch, "fix/bug-123");
    }

    #[test]
    fn test_selected_worktree_mut() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.selected_worktree = Some((1, 0));
        let wt = app.selected_worktree_mut().unwrap();
        wt.status = WorktreeStatus::Running;

        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.status, WorktreeStatus::Running);
    }

    #[test]
    fn test_selected_worktree_out_of_bounds() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.selected_worktree = Some((99, 0));
        assert!(app.selected_worktree().is_none());
    }

    #[test]
    fn test_show_error() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_error("テストエラー".to_string());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "テストエラー");
        assert_eq!(msg.level, StatusLevel::Error);
    }

    #[test]
    fn test_show_info() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_info("情報メッセージ".to_string());
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.text, "情報メッセージ");
        assert_eq!(msg.level, StatusLevel::Info);
    }

    #[test]
    fn test_worktree_status_icon() {
        assert_eq!(WorktreeStatus::Running.icon(), "●");
        assert_eq!(WorktreeStatus::Idle.icon(), "○");
        assert_eq!(WorktreeStatus::Done.icon(), "✓");
    }

    #[test]
    fn test_empty_projects_config() {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
                base_branch: None,
            },
            projects: vec![],
        };
        let app = App::new(&config);
        assert!(app.projects.is_empty());
    }

    #[test]
    fn test_worktree_by_id() {
        let config = sample_config();
        let app = App::new(&config);

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.name, "feature-auth");

        let wt = app.worktree_by_id((1, 0)).unwrap();
        assert_eq!(wt.name, "refactor");

        assert!(app.worktree_by_id((99, 0)).is_none());
    }

    #[test]
    fn test_worktree_by_id_mut() {
        let config = sample_config();
        let mut app = App::new(&config);

        let wt = app.worktree_by_id_mut((0, 1)).unwrap();
        wt.status = WorktreeStatus::Running;

        assert_eq!(
            app.worktree_by_id((0, 1)).unwrap().status,
            WorktreeStatus::Running
        );
    }

    #[test]
    fn test_handle_claude_content_delta_new_message() {
        let config = sample_config();
        let mut app = App::new(&config);
        app.selected_worktree = Some((0, 0));

        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "Hello".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 1);
        assert_eq!(wt.chat_history[0].role, Role::Assistant);
        assert_eq!(wt.chat_history[0].content, "Hello");
    }

    #[test]
    fn test_handle_claude_content_delta_append() {
        let config = sample_config();
        let mut app = App::new(&config);

        // 先にアシスタントメッセージを追加
        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "He".to_string(),
            },
        );
        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "llo".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 1);
        assert_eq!(wt.chat_history[0].content, "Hello");
    }

    #[test]
    fn test_handle_claude_content_delta_after_user_message() {
        let config = sample_config();
        let mut app = App::new(&config);

        // ユーザーメッセージがある状態
        if let Some(wt) = app.worktree_by_id_mut((0, 0)) {
            wt.chat_history.push(ChatMessage {
                role: Role::User,
                content: "Hi".to_string(),
                timestamp: Utc::now(),
            });
        }

        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "Hello!".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 2);
        assert_eq!(wt.chat_history[0].role, Role::User);
        assert_eq!(wt.chat_history[1].role, Role::Assistant);
        assert_eq!(wt.chat_history[1].content, "Hello!");
    }

    #[test]
    fn test_handle_claude_tool_use_new_message() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ToolUse {
                tool: "Read".to_string(),
                input: serde_json::json!({"path": "/tmp/test.rs"}),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 1);
        assert!(wt.chat_history[0].content.contains("[Tool: Read]"));
    }

    #[test]
    fn test_handle_claude_tool_use_append() {
        let config = sample_config();
        let mut app = App::new(&config);

        // 先にテキストを追加
        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "Let me check".to_string(),
            },
        );
        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ToolUse {
                tool: "Bash".to_string(),
                input: serde_json::Value::Null,
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 1);
        assert!(wt.chat_history[0].content.contains("Let me check"));
        assert!(wt.chat_history[0].content.contains("[Tool: Bash]"));
    }

    #[test]
    fn test_handle_claude_init() {
        let config = sample_config();
        let mut app = App::new(&config);

        // Init イベントはチャットに何も追加しない
        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::Init {
                session_id: "test-session".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert!(wt.chat_history.is_empty());
    }

    #[test]
    fn test_handle_claude_complete() {
        let config = sample_config();
        let mut app = App::new(&config);

        // ステータスを Running にしておく
        app.worktree_by_id_mut((0, 0)).unwrap().status = WorktreeStatus::Running;

        app.handle_claude_complete((0, 0));

        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().status,
            WorktreeStatus::Idle
        );
    }

    #[test]
    fn test_handle_claude_complete_invalid_id() {
        let config = sample_config();
        let mut app = App::new(&config);

        // 存在しない worktree ID → パニックしない
        app.handle_claude_complete((99, 99));
    }

    #[test]
    fn test_handle_claude_error() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.worktree_by_id_mut((0, 0)).unwrap().status = WorktreeStatus::Running;

        app.handle_claude_error((0, 0), "API rate limit");

        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().status,
            WorktreeStatus::Idle
        );
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, StatusLevel::Error);
        assert!(msg.text.contains("API rate limit"));
    }

    #[test]
    fn test_handle_claude_error_invalid_id() {
        let config = sample_config();
        let mut app = App::new(&config);

        // 存在しない ID でもエラーメッセージは設定される
        app.handle_claude_error((99, 99), "error");

        assert!(app.status_message.is_some());
    }

    #[test]
    fn test_handle_claude_result_with_text() {
        let config = sample_config();
        let mut app = App::new(&config);

        // 空のアシスタントメッセージがある場合
        if let Some(wt) = app.worktree_by_id_mut((0, 0)) {
            wt.chat_history.push(ChatMessage {
                role: Role::Assistant,
                content: String::new(),
                timestamp: Utc::now(),
            });
        }

        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::Result {
                text: "完了しました".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history[0].content, "完了しました");
    }

    #[test]
    fn test_handle_claude_scroll_offset_auto_scroll() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.handle_claude_output(
            (0, 0),
            ClaudeStreamEvent::ContentDelta {
                text: "test".to_string(),
            },
        );

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_scroll_offset, usize::MAX);
    }

    // --- ステータスメッセージ自動クリアテスト ---

    #[test]
    fn test_show_error_sets_timestamp() {
        let config = sample_config();
        let mut app = App::new(&config);

        assert!(app.status_set_at.is_none());
        app.show_error("error".to_string());
        assert!(app.status_set_at.is_some());
    }

    #[test]
    fn test_show_info_sets_timestamp() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_info("info".to_string());
        assert!(app.status_set_at.is_some());
        assert!(app.status_message.is_some());
    }

    #[test]
    fn test_clear_expired_status_not_expired() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_info("hello".to_string());
        // 直後はまだクリアされない
        app.clear_expired_status();
        assert!(app.status_message.is_some());
    }

    #[test]
    fn test_clear_expired_status_expired() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_info("hello".to_string());
        // タイムスタンプを過去に設定して期限切れをシミュレート
        app.status_set_at = Some(Instant::now() - Duration::from_secs(10));
        app.clear_expired_status();
        assert!(app.status_message.is_none());
        assert!(app.status_set_at.is_none());
    }

    #[test]
    fn test_clear_expired_status_no_message() {
        let config = sample_config();
        let mut app = App::new(&config);

        // メッセージがない場合はパニックしない
        app.clear_expired_status();
        assert!(app.status_message.is_none());
    }

    #[test]
    fn test_new_message_resets_timer() {
        let config = sample_config();
        let mut app = App::new(&config);

        app.show_error("first".to_string());
        let first_time = app.status_set_at.unwrap();

        // 少し待ってから新しいメッセージを設定
        std::thread::sleep(Duration::from_millis(10));
        app.show_info("second".to_string());
        let second_time = app.status_set_at.unwrap();

        assert!(second_time > first_time);
        assert_eq!(app.status_message.as_ref().unwrap().text, "second");
    }
}

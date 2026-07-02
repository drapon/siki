use crate::app::{App, Project, WorktreeId};
use crate::session::SessionRegistry;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

/// フラット化リストの各行の種類
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    /// プロジェクトヘッダ行
    Project { index: usize },
    /// Worktree 行
    Worktree { project_index: usize, worktree_index: usize },
}

/// 左パネルの状態
#[derive(Debug)]
pub struct LeftPanel {
    pub cursor_index: usize,
    pub scroll_offset: usize,
}

impl LeftPanel {
    pub fn new() -> Self {
        Self {
            cursor_index: 0,
            scroll_offset: 0,
        }
    }

    /// プロジェクト一覧からフラット化リストを構築する
    pub fn build_entries(projects: &[Project]) -> Vec<ListEntry> {
        let mut entries = Vec::new();
        for (pi, project) in projects.iter().enumerate() {
            entries.push(ListEntry::Project { index: pi });
            if !project.collapsed {
                for (wi, _wt) in project.worktrees.iter().enumerate() {
                    entries.push(ListEntry::Worktree {
                        project_index: pi,
                        worktree_index: wi,
                    });
                }
            }
        }
        entries
    }

    /// カーソルを下に移動
    pub fn move_down(&mut self, entries_len: usize) {
        if entries_len == 0 {
            return;
        }
        if self.cursor_index < entries_len - 1 {
            self.cursor_index += 1;
        }
    }

    /// カーソルを上に移動
    pub fn move_up(&mut self) {
        if self.cursor_index > 0 {
            self.cursor_index -= 1;
        }
    }

    /// カーソル位置をエントリ数に制限する（折りたたみ後などに呼ぶ）
    pub fn clamp_cursor(&mut self, entries_len: usize) {
        if entries_len == 0 {
            self.cursor_index = 0;
        } else if self.cursor_index >= entries_len {
            self.cursor_index = entries_len - 1;
        }
    }

    /// 現在のカーソル位置のエントリを取得
    pub fn current_entry<'a>(&self, entries: &'a [ListEntry]) -> Option<&'a ListEntry> {
        entries.get(self.cursor_index)
    }

    /// プロジェクトの折りたたみ/展開を切り替える
    pub fn toggle_collapse(&self, app: &mut App, entries: &[ListEntry]) {
        if let Some(ListEntry::Project { index }) = self.current_entry(entries) {
            app.projects[*index].collapsed = !app.projects[*index].collapsed;
        }
    }

    /// Worktree を選択する（カーソルが worktree 行にある場合）
    pub fn select_worktree(&self, entries: &[ListEntry]) -> Option<WorktreeId> {
        match self.current_entry(entries)? {
            ListEntry::Worktree {
                project_index,
                worktree_index,
            } => Some((*project_index, *worktree_index)),
            ListEntry::Project { .. } => None,
        }
    }

    /// 左パネルを描画する
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        app: &App,
        focused: bool,
        session_registry: Option<&SessionRegistry>,
    ) {
        let entries = Self::build_entries(&app.projects);

        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_cursor = i == self.cursor_index;

                match entry {
                    ListEntry::Project { index } => {
                        let project = &app.projects[*index];
                        let arrow = if project.collapsed { "▸" } else { "▾" };
                        let display = project.display_name.as_deref().unwrap_or(&project.name);
                        let text = format!("{} {}", arrow, display);
                        let mut style = Style::default().bold();
                        if is_cursor {
                            style = style.bg(Color::DarkGray);
                        }
                        ListItem::new(text).style(style)
                    }
                    ListEntry::Worktree {
                        project_index,
                        worktree_index,
                    } => {
                        let wt = &app.projects[*project_index].worktrees[*worktree_index];
                        let project_name = &app.projects[*project_index].name;
                        let is_last = *worktree_index
                            == app.projects[*project_index].worktrees.len() - 1;
                        let branch_char = if is_last { "└" } else { "├" };
                        // セッションレジストリから状態バッジを取得（なければ既存アイコン）
                        let has_alert = session_registry
                            .map(|reg| reg.has_alert(project_name, &wt.name))
                            .unwrap_or(false);
                        let session_state = session_registry
                            .and_then(|reg| reg.aggregate_state(project_name, &wt.name));
                        let (icon, icon_color) = if has_alert {
                            ("●", Color::Red)
                        } else if let Some(state) = session_state {
                            (state.badge_char(app.blink_phase), state.badge_color())
                        } else {
                            ("○", Color::DarkGray)
                        };

                        let is_selected = app.selected_worktree == Some((*project_index, *worktree_index));
                        let name_fg = if is_selected {
                            Color::Green
                        } else {
                            Color::Reset
                        };
                        let branch_fg = if is_cursor {
                            Color::Gray
                        } else {
                            Color::DarkGray
                        };

                        let prefix = format!("  {} ", branch_char);
                        let display = wt.display_name.as_deref().unwrap_or(&wt.name);
                        let name_part = format!(" {} ", display);
                        let branch_part = format!(" {}", wt.branch);
                        let mut spans = vec![
                            Span::styled(prefix, Style::default().fg(name_fg)),
                            Span::styled(icon, Style::default().fg(icon_color)),
                            Span::styled(name_part, Style::default().fg(name_fg)),
                            Span::styled(branch_part, Style::default().fg(branch_fg).dim()),
                        ];
                        if let Some(ref pr) = wt.pr {
                            spans.push(Span::styled(
                                format!(" ({})", pr.title),
                                Style::default().fg(Color::Yellow).dim(),
                            ));
                        }
                        let line = Line::from(spans);

                        let mut item_style = Style::default();
                        if is_cursor {
                            item_style = item_style.bg(Color::DarkGray);
                        }
                        ListItem::new(line).style(item_style)
                    }
                }
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Projects")
            .border_style(if focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let list = List::new(items).block(block);

        // ListState でスクロール位置を管理
        let mut list_state = ListState::default();
        list_state.select(Some(self.cursor_index));

        frame.render_stateful_widget(list, area, &mut list_state);

        // 描画後のスクロールオフセットを保存（マウスクリック時の行計算に使用）
        self.scroll_offset = list_state.offset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Project, Worktree, RightPanelMode, DiffFocus};
    use std::path::PathBuf;

    fn sample_projects() -> Vec<Project> {
        vec![
            Project {
                name: "webapp".to_string(),
                display_name: None,
                path: PathBuf::from("/home/user/webapp"),
                collapsed: false,
                worktrees: vec![
                    Worktree {
                        name: "feature-auth".to_string(),
                        display_name: None,
                        parent: None,
                        branch: "feature/auth".to_string(),
                        path: PathBuf::from("/tmp/wt1"),

                        chat_history: vec![],
                        open_files: vec![],
                        active_tab: 0,
                        claude_tabs: 0,
                        claude_tab_llm: Vec::new(),
                        right_panel_mode: RightPanelMode::Tree,
                        diff_focus: DiffFocus::PrDiff,
                        active_terminal: 0,
                        chat_scroll_offset: 0,
                        claude_scroll_offsets: std::collections::HashMap::new(),
                        pr: None,
                        claude_session_id: None,
                        context_items: Vec::new(),
                        context_cursor: 0,
                    },
                    Worktree {
                        name: "fix-bug".to_string(),
                        display_name: None,
                        parent: None,
                        branch: "fix/bug-123".to_string(),
                        path: PathBuf::from("/tmp/wt2"),

                        chat_history: vec![],
                        open_files: vec![],
                        active_tab: 0,
                        claude_tabs: 0,
                        claude_tab_llm: Vec::new(),
                        right_panel_mode: RightPanelMode::Tree,
                        diff_focus: DiffFocus::PrDiff,
                        active_terminal: 0,
                        chat_scroll_offset: 0,
                        claude_scroll_offsets: std::collections::HashMap::new(),
                        pr: None,
                        claude_session_id: None,
                        context_items: Vec::new(),
                        context_cursor: 0,
                    },
                ],
            },
            Project {
                name: "api".to_string(),
                display_name: None,
                path: PathBuf::from("/home/user/api"),
                collapsed: false,
                worktrees: vec![Worktree {
                    name: "refactor".to_string(),
                    display_name: None,
                    parent: None,
                    branch: "refactor/db".to_string(),
                    path: PathBuf::from("/tmp/wt3"),

                    chat_history: vec![],
                    open_files: vec![],
                    active_tab: 0,
                    claude_tabs: 0,
                    claude_tab_llm: Vec::new(),
                    right_panel_mode: RightPanelMode::Tree,
                    diff_focus: DiffFocus::PrDiff,
                    active_terminal: 0,
                    chat_scroll_offset: 0,
                    claude_scroll_offsets: std::collections::HashMap::new(),
                    pr: None,
                    claude_session_id: None,
                    context_items: Vec::new(),
                    context_cursor: 0,
                }],
            },
        ]
    }

    #[test]
    fn test_build_entries_all_expanded() {
        let projects = sample_projects();
        let entries = LeftPanel::build_entries(&projects);

        // webapp(project) + feature-auth + fix-bug + api(project) + refactor = 5
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0], ListEntry::Project { index: 0 });
        assert_eq!(
            entries[1],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 0
            }
        );
        assert_eq!(
            entries[2],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 1
            }
        );
        assert_eq!(entries[3], ListEntry::Project { index: 1 });
        assert_eq!(
            entries[4],
            ListEntry::Worktree {
                project_index: 1,
                worktree_index: 0
            }
        );
    }

    #[test]
    fn test_build_entries_first_collapsed() {
        let mut projects = sample_projects();
        projects[0].collapsed = true;
        let entries = LeftPanel::build_entries(&projects);

        // webapp(project, collapsed) + api(project) + refactor = 3
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ListEntry::Project { index: 0 });
        assert_eq!(entries[1], ListEntry::Project { index: 1 });
        assert_eq!(
            entries[2],
            ListEntry::Worktree {
                project_index: 1,
                worktree_index: 0
            }
        );
    }

    #[test]
    fn test_build_entries_all_collapsed() {
        let mut projects = sample_projects();
        projects[0].collapsed = true;
        projects[1].collapsed = true;
        let entries = LeftPanel::build_entries(&projects);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ListEntry::Project { index: 0 });
        assert_eq!(entries[1], ListEntry::Project { index: 1 });
    }

    #[test]
    fn test_build_entries_empty() {
        let entries = LeftPanel::build_entries(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_move_down() {
        let mut panel = LeftPanel::new();
        assert_eq!(panel.cursor_index, 0);

        panel.move_down(5);
        assert_eq!(panel.cursor_index, 1);

        panel.move_down(5);
        assert_eq!(panel.cursor_index, 2);
    }

    #[test]
    fn test_move_down_at_bottom() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 4;

        panel.move_down(5);
        assert_eq!(panel.cursor_index, 4); // 動かない
    }

    #[test]
    fn test_move_down_empty_list() {
        let mut panel = LeftPanel::new();
        panel.move_down(0);
        assert_eq!(panel.cursor_index, 0);
    }

    #[test]
    fn test_move_up() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 3;

        panel.move_up();
        assert_eq!(panel.cursor_index, 2);

        panel.move_up();
        assert_eq!(panel.cursor_index, 1);
    }

    #[test]
    fn test_move_up_at_top() {
        let mut panel = LeftPanel::new();
        panel.move_up();
        assert_eq!(panel.cursor_index, 0); // 動かない
    }

    #[test]
    fn test_clamp_cursor() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 10;

        panel.clamp_cursor(5);
        assert_eq!(panel.cursor_index, 4);
    }

    #[test]
    fn test_clamp_cursor_empty() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 5;

        panel.clamp_cursor(0);
        assert_eq!(panel.cursor_index, 0);
    }

    #[test]
    fn test_clamp_cursor_within_bounds() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 2;

        panel.clamp_cursor(5);
        assert_eq!(panel.cursor_index, 2); // そのまま
    }

    #[test]
    fn test_current_entry() {
        let panel = LeftPanel::new();
        let projects = sample_projects();
        let entries = LeftPanel::build_entries(&projects);

        let entry = panel.current_entry(&entries).unwrap();
        assert_eq!(*entry, ListEntry::Project { index: 0 });
    }

    #[test]
    fn test_current_entry_at_worktree() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 1;
        let projects = sample_projects();
        let entries = LeftPanel::build_entries(&projects);

        let entry = panel.current_entry(&entries).unwrap();
        assert_eq!(
            *entry,
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 0
            }
        );
    }

    #[test]
    fn test_current_entry_empty() {
        let panel = LeftPanel::new();
        let entries: Vec<ListEntry> = vec![];

        assert!(panel.current_entry(&entries).is_none());
    }

    #[test]
    fn test_select_worktree_on_worktree_row() {
        let mut panel = LeftPanel::new();
        panel.cursor_index = 2; // fix-bug (project_index=0, worktree_index=1)
        let projects = sample_projects();
        let entries = LeftPanel::build_entries(&projects);

        let wt_id = panel.select_worktree(&entries);
        assert_eq!(wt_id, Some((0, 1)));
    }

    #[test]
    fn test_select_worktree_on_project_row() {
        let panel = LeftPanel::new(); // cursor_index=0 = project header
        let projects = sample_projects();
        let entries = LeftPanel::build_entries(&projects);

        let wt_id = panel.select_worktree(&entries);
        assert_eq!(wt_id, None);
    }

    #[test]
    fn test_toggle_collapse() {
        use crate::config::{Config, SikiConfig, ProjectConfig, WorktreeConfig};

        let config = Config {
            siki: SikiConfig::default(),
            projects: vec![
                ProjectConfig {
                    name: "webapp".to_string(),
                    path: "/tmp/webapp".to_string(),
                    display_name: None,
                    worktrees: vec![WorktreeConfig {
                        name: "wt1".to_string(),
                        branch: "main".to_string(),
                    }],
                },
            ],
        };
        let mut app = App::new(&config);
        assert!(!app.projects[0].collapsed);

        let panel = LeftPanel::new(); // cursor at project header
        let entries = LeftPanel::build_entries(&app.projects);
        panel.toggle_collapse(&mut app, &entries);

        assert!(app.projects[0].collapsed);

        // もう一度トグル
        let entries = LeftPanel::build_entries(&app.projects);
        panel.toggle_collapse(&mut app, &entries);
        assert!(!app.projects[0].collapsed);
    }

    #[test]
    fn test_toggle_collapse_on_worktree_does_nothing() {
        use crate::config::{Config, SikiConfig, ProjectConfig, WorktreeConfig};

        let config = Config {
            siki: SikiConfig::default(),
            projects: vec![ProjectConfig {
                name: "webapp".to_string(),
                path: "/tmp/webapp".to_string(),
                display_name: None,
                worktrees: vec![WorktreeConfig {
                    name: "wt1".to_string(),
                    branch: "main".to_string(),
                }],
            }],
        };
        let mut app = App::new(&config);

        let mut panel = LeftPanel::new();
        panel.cursor_index = 1; // worktree 行
        let entries = LeftPanel::build_entries(&app.projects);
        panel.toggle_collapse(&mut app, &entries);

        assert!(!app.projects[0].collapsed); // 変わらない
    }

    #[test]
    fn test_cursor_clamp_after_collapse() {
        let mut projects = sample_projects();
        // webapp: 2 worktrees, api: 1 worktree → 5 entries total
        let entries = LeftPanel::build_entries(&projects);
        assert_eq!(entries.len(), 5);

        let mut panel = LeftPanel::new();
        panel.cursor_index = 2; // fix-bug (2nd worktree of webapp)

        // collapse webapp → entries become: webapp(collapsed), api, refactor = 3
        projects[0].collapsed = true;
        let entries = LeftPanel::build_entries(&projects);
        assert_eq!(entries.len(), 3);

        panel.clamp_cursor(entries.len());
        assert_eq!(panel.cursor_index, 2); // still valid (now points to "refactor")
    }

    #[test]
    fn test_cursor_clamp_after_collapse_overflow() {
        let mut projects = sample_projects();
        let mut panel = LeftPanel::new();
        panel.cursor_index = 4; // last entry (refactor)

        // collapse both
        projects[0].collapsed = true;
        projects[1].collapsed = true;
        let entries = LeftPanel::build_entries(&projects);
        assert_eq!(entries.len(), 2);

        panel.clamp_cursor(entries.len());
        assert_eq!(panel.cursor_index, 1); // clamped to last valid
    }
}

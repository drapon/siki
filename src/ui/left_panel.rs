use crate::app::{App, Project, Worktree, WorktreeId};
use crate::session::{SessionRegistry, SessionState};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use std::collections::{HashMap, HashSet};

/// フラット化リストの各行の種類
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    /// プロジェクトヘッダ行
    Project { index: usize },
    /// Worktree 行
    Worktree {
        project_index: usize,
        worktree_index: usize,
        depth: usize,
        is_last: bool,
    },
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
                let mut children_of: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
                for (wi, wt) in project.worktrees.iter().enumerate() {
                    children_of.entry(wt.parent.as_deref()).or_default().push(wi);
                }
                let mut visited = HashSet::new();
                Self::push_worktree_entries(
                    pi,
                    None,
                    0,
                    &project.worktrees,
                    &children_of,
                    &mut entries,
                    &mut visited,
                );
                let remaining: Vec<usize> = (0..project.worktrees.len())
                    .filter(|wi| !visited.contains(wi))
                    .collect();
                for (position, wi) in remaining.iter().enumerate() {
                    if !visited.insert(*wi) {
                        continue;
                    }
                    entries.push(ListEntry::Worktree {
                        project_index: pi,
                        worktree_index: *wi,
                        depth: 0,
                        is_last: position == remaining.len() - 1,
                    });
                    Self::push_worktree_entries(
                        pi,
                        Some(project.worktrees[*wi].name.as_str()),
                        1,
                        &project.worktrees,
                        &children_of,
                        &mut entries,
                        &mut visited,
                    );
                }
            }
        }
        entries
    }

    fn push_worktree_entries<'a>(
        project_index: usize,
        parent_key: Option<&'a str>,
        depth: usize,
        worktrees: &'a [Worktree],
        children_of: &HashMap<Option<&'a str>, Vec<usize>>,
        entries: &mut Vec<ListEntry>,
        visited: &mut HashSet<usize>,
    ) {
        let Some(children) = children_of.get(&parent_key) else {
            return;
        };
        for (position, wi) in children.iter().enumerate() {
            if !visited.insert(*wi) {
                continue;
            }
            entries.push(ListEntry::Worktree {
                project_index,
                worktree_index: *wi,
                depth,
                is_last: position == children.len() - 1,
            });
            Self::push_worktree_entries(
                project_index,
                Some(worktrees[*wi].name.as_str()),
                depth + 1,
                worktrees,
                children_of,
                entries,
                visited,
            );
        }
    }

    fn descendants_of(worktrees: &[Worktree], root: &str) -> Vec<usize> {
        let mut children_of: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        for (wi, wt) in worktrees.iter().enumerate() {
            children_of.entry(wt.parent.as_deref()).or_default().push(wi);
        }

        let mut descendants = Vec::new();
        let mut visited = HashSet::new();
        Self::push_descendants(
            root,
            Some(root),
            worktrees,
            &children_of,
            &mut descendants,
            &mut visited,
        );
        descendants
    }

    fn push_descendants<'a>(
        root: &str,
        parent_key: Option<&'a str>,
        worktrees: &'a [Worktree],
        children_of: &HashMap<Option<&'a str>, Vec<usize>>,
        descendants: &mut Vec<usize>,
        visited: &mut HashSet<usize>,
    ) {
        let Some(children) = children_of.get(&parent_key) else {
            return;
        };
        for wi in children {
            if worktrees[*wi].name == root || !visited.insert(*wi) {
                continue;
            }
            descendants.push(*wi);
            Self::push_descendants(
                root,
                Some(worktrees[*wi].name.as_str()),
                worktrees,
                children_of,
                descendants,
                visited,
            );
        }
    }

    fn compute_badge(
        project_name: &str,
        worktrees: &[Worktree],
        worktree_index: usize,
        session_registry: Option<&SessionRegistry>,
    ) -> (bool, Option<SessionState>) {
        let wt = &worktrees[worktree_index];
        let own_alert = session_registry
            .map(|reg| reg.has_alert(project_name, &wt.name))
            .unwrap_or(false);
        let own_state = session_registry
            .and_then(|reg| reg.aggregate_state(project_name, &wt.name));
        let Some(reg) = session_registry else {
            return (own_alert, own_state);
        };

        let mut merged_alert = own_alert;
        let mut merged_state = own_state;
        for descendant_index in Self::descendants_of(worktrees, &wt.name) {
            let descendant_name = &worktrees[descendant_index].name;
            merged_alert |= reg.has_alert(project_name, descendant_name);
            merged_state = [merged_state, reg.aggregate_state(project_name, descendant_name)]
                .into_iter()
                .flatten()
                .max_by_key(|state| state.priority());
        }

        (merged_alert, merged_state)
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
                ..
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
                        depth,
                        is_last,
                    } => {
                        let wt = &app.projects[*project_index].worktrees[*worktree_index];
                        let project_name = &app.projects[*project_index].name;
                        let branch_char = if *is_last { "└" } else { "├" };
                        // セッションレジストリから状態バッジを取得（なければ既存アイコン）
                        let (has_alert, session_state) = Self::compute_badge(
                            project_name,
                            &app.projects[*project_index].worktrees,
                            *worktree_index,
                            session_registry,
                        );
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

                        let prefix = Self::worktree_prefix(*depth, branch_char);
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

    fn worktree_prefix(depth: usize, branch_char: &str) -> String {
        format!("{}{} ", "  ".repeat(depth + 1), branch_char)
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

    fn test_worktree(name: &str, parent: Option<&str>) -> Worktree {
        Worktree {
            name: name.to_string(),
            display_name: None,
            parent: parent.map(|p| p.to_string()),
            branch: format!("branch/{}", name),
            path: PathBuf::from(format!("/tmp/{}", name)),
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
        }
    }

    fn project_with_worktrees(worktrees: Vec<Worktree>) -> Project {
        Project {
            name: "tree".to_string(),
            display_name: None,
            path: PathBuf::from("/tmp/tree"),
            collapsed: false,
            worktrees,
        }
    }

    fn register_session(
        reg: &mut SessionRegistry,
        id: &str,
        project: &str,
        worktree: &str,
        state: SessionState,
    ) {
        let cwd = format!(
            "{}/{}/{}",
            crate::config::workspaces_dir().display(),
            project,
            worktree
        );
        reg.register(id.to_string(), cwd, "default".to_string());
        reg.update_state(id, state);
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
                worktree_index: 0,
                depth: 0,
                is_last: false
            }
        );
        assert_eq!(
            entries[2],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 1,
                depth: 0,
                is_last: true
            }
        );
        assert_eq!(entries[3], ListEntry::Project { index: 1 });
        assert_eq!(
            entries[4],
            ListEntry::Worktree {
                project_index: 1,
                worktree_index: 0,
                depth: 0,
                is_last: true
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
                worktree_index: 0,
                depth: 0,
                is_last: true
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
    fn test_build_entries_parent_dfs_depth() {
        let project = project_with_worktrees(vec![
            test_worktree("A", None),
            test_worktree("B", Some("A")),
            test_worktree("C", Some("B")),
        ]);
        let entries = LeftPanel::build_entries(&[project]);

        assert_eq!(
            entries[1],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 0,
                depth: 0,
                is_last: true
            }
        );
        assert_eq!(
            entries[2],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 1,
                depth: 1,
                is_last: true
            }
        );
        assert_eq!(
            entries[3],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 2,
                depth: 2,
                is_last: true
            }
        );
    }

    #[test]
    fn test_build_entries_flat_order_and_depth_regression() {
        let project = project_with_worktrees(vec![
            test_worktree("X", None),
            test_worktree("Y", None),
            test_worktree("Z", None),
        ]);
        let entries = LeftPanel::build_entries(&[project]);
        let worktrees: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                ListEntry::Worktree { worktree_index, depth, .. } => Some((*worktree_index, *depth)),
                ListEntry::Project { .. } => None,
            })
            .collect();

        assert_eq!(worktrees, vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn test_build_entries_keeps_dangling_parent_as_root() {
        let project = project_with_worktrees(vec![
            test_worktree("A", None),
            test_worktree("B", Some("missing")),
        ]);
        let entries = LeftPanel::build_entries(&[project]);

        assert_eq!(
            entries[2],
            ListEntry::Worktree {
                project_index: 0,
                worktree_index: 1,
                depth: 0,
                is_last: true
            }
        );
    }

    #[test]
    fn test_build_entries_is_last_by_siblings() {
        let project = project_with_worktrees(vec![
            test_worktree("A", None),
            test_worktree("B", Some("A")),
            test_worktree("C", Some("A")),
            test_worktree("D", None),
            test_worktree("E", Some("D")),
        ]);
        let entries = LeftPanel::build_entries(&[project]);

        assert!(matches!(entries[1], ListEntry::Worktree { worktree_index: 0, is_last: false, .. }));
        assert!(matches!(entries[2], ListEntry::Worktree { worktree_index: 1, is_last: false, .. }));
        assert!(matches!(entries[3], ListEntry::Worktree { worktree_index: 2, is_last: true, .. }));
        assert!(matches!(entries[4], ListEntry::Worktree { worktree_index: 3, is_last: true, .. }));
        assert!(matches!(entries[5], ListEntry::Worktree { worktree_index: 4, is_last: true, .. }));
    }

    #[test]
    fn test_build_entries_cycle_does_not_recurse_forever() {
        let project = project_with_worktrees(vec![
            test_worktree("X", Some("Y")),
            test_worktree("Y", Some("X")),
        ]);
        let entries = LeftPanel::build_entries(&[project]);
        let worktree_indices: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                ListEntry::Worktree { worktree_index, .. } => Some(*worktree_index),
                ListEntry::Project { .. } => None,
            })
            .collect();

        assert_eq!(worktree_indices, vec![0, 1]);
    }

    #[test]
    fn test_descendants_of_resolves_children_and_grandchildren() {
        let worktrees = vec![
            test_worktree("A", None),
            test_worktree("B", Some("A")),
            test_worktree("C", Some("B")),
            test_worktree("D", Some("A")),
        ];

        assert_eq!(LeftPanel::descendants_of(&worktrees, "A"), vec![1, 2, 3]);
    }

    #[test]
    fn test_descendants_of_cycle_does_not_recurse_forever() {
        let worktrees = vec![
            test_worktree("X", Some("Y")),
            test_worktree("Y", Some("X")),
        ];

        assert_eq!(LeftPanel::descendants_of(&worktrees, "X"), vec![1]);
    }

    #[test]
    fn test_compute_badge_childless_matches_own_state_and_alert() {
        let worktrees = vec![
            test_worktree("idle", None),
            test_worktree("working", None),
            test_worktree("alert", None),
        ];
        let mut reg = SessionRegistry::new();
        register_session(&mut reg, "s-idle", "tree", "idle", SessionState::Idle);
        register_session(&mut reg, "s-working", "tree", "working", SessionState::Working);
        register_session(&mut reg, "s-alert", "tree", "alert", SessionState::Done);
        reg.set_alert("s-alert", true, Some("needs review".to_string()));

        for (index, wt) in worktrees.iter().enumerate() {
            let badge = LeftPanel::compute_badge("tree", &worktrees, index, Some(&reg));
            let own = (
                reg.has_alert("tree", &wt.name),
                reg.aggregate_state("tree", &wt.name),
            );
            assert_eq!(badge, own);
        }
    }

    #[test]
    fn test_compute_badge_rolls_up_child_working_to_idle_parent() {
        let worktrees = vec![
            test_worktree("parent", None),
            test_worktree("child", Some("parent")),
        ];
        let mut reg = SessionRegistry::new();
        register_session(&mut reg, "s-parent", "tree", "parent", SessionState::Idle);
        register_session(&mut reg, "s-child", "tree", "child", SessionState::Working);

        let (_, state) = LeftPanel::compute_badge("tree", &worktrees, 0, Some(&reg));
        assert_eq!(state, Some(SessionState::Working));
    }

    #[test]
    fn test_compute_badge_does_not_downgrade_parent_working() {
        let worktrees = vec![
            test_worktree("parent", None),
            test_worktree("child", Some("parent")),
        ];
        let mut reg = SessionRegistry::new();
        register_session(&mut reg, "s-parent", "tree", "parent", SessionState::Working);
        register_session(&mut reg, "s-child", "tree", "child", SessionState::Idle);

        let (_, state) = LeftPanel::compute_badge("tree", &worktrees, 0, Some(&reg));
        assert_eq!(state, Some(SessionState::Working));
    }

    #[test]
    fn test_compute_badge_rolls_up_child_alert_with_or() {
        let worktrees = vec![
            test_worktree("parent", None),
            test_worktree("child", Some("parent")),
        ];
        let mut reg = SessionRegistry::new();
        register_session(&mut reg, "s-parent", "tree", "parent", SessionState::Idle);
        register_session(&mut reg, "s-child", "tree", "child", SessionState::Idle);

        assert!(!LeftPanel::compute_badge("tree", &worktrees, 0, Some(&reg)).0);

        reg.set_alert("s-child", true, Some("blocked".to_string()));
        assert!(LeftPanel::compute_badge("tree", &worktrees, 0, Some(&reg)).0);
    }

    #[test]
    fn test_compute_badge_rolls_up_grandchild_state_and_alert() {
        let worktrees = vec![
            test_worktree("A", None),
            test_worktree("B", Some("A")),
            test_worktree("C", Some("B")),
        ];
        let mut reg = SessionRegistry::new();
        register_session(&mut reg, "s-a", "tree", "A", SessionState::Idle);
        register_session(&mut reg, "s-b", "tree", "B", SessionState::Idle);
        register_session(&mut reg, "s-c", "tree", "C", SessionState::Working);
        reg.set_alert("s-c", true, Some("needs review".to_string()));

        let (alert, state) = LeftPanel::compute_badge("tree", &worktrees, 0, Some(&reg));
        assert!(alert);
        assert_eq!(state, Some(SessionState::Working));
    }

    #[test]
    fn test_worktree_prefix_depth_zero_matches_existing_bytes() {
        let branch_char = "└";
        let old_prefix = format!("  {} ", branch_char);

        assert_eq!(LeftPanel::worktree_prefix(0, branch_char).as_bytes(), old_prefix.as_bytes());
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
                worktree_index: 0,
                depth: 0,
                is_last: false
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

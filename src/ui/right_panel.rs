use crate::app::{App, DiffFocus, RightPanelMode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs};

use super::diff_view::{DiffView, LocalChangesView};
use super::source_tree::SourceTree;

/// 右パネル上部を描画する
pub fn render_top(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    source_tree: &mut SourceTree,
    diff_view: &DiffView,
    local_changes: &LocalChangesView,
    focused: bool,
) {
    match app.selected_worktree() {
        Some(wt) => {
            let chunks = Layout::vertical([
                Constraint::Length(1), // タブバー
                Constraint::Min(0),   // コンテンツ
            ])
            .split(area);

            // タブバー描画
            let active = match wt.right_panel_mode {
                RightPanelMode::Tree => 0,
                RightPanelMode::Diff => 1,
                RightPanelMode::Context => 2,
            };
            let tabs = Tabs::new(vec!["Tree", "Changes", "Context"])
                .select(active)
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .style(if focused {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                })
                .divider("|");
            frame.render_widget(tabs, chunks[0]);

            // コンテンツ描画
            match wt.right_panel_mode {
                RightPanelMode::Tree => {
                    source_tree.render(frame, chunks[1], focused);
                }
                RightPanelMode::Diff => {
                    // PR差分（上）とローカル変更（下）の2分割
                    let diff_chunks = Layout::vertical([
                        Constraint::Percentage(50),
                        Constraint::Percentage(50),
                    ])
                    .split(chunks[1]);

                    let pr_focused = focused && wt.diff_focus == DiffFocus::PrDiff;
                    let local_focused = focused && wt.diff_focus == DiffFocus::LocalChanges;

                    diff_view.render(frame, diff_chunks[0], pr_focused, Some("PR Changes"));
                    local_changes.render(frame, diff_chunks[1], local_focused, Some("Local"));
                }
                RightPanelMode::Context => {
                    render_context_list(frame, chunks[1], &wt.context_items, wt.context_cursor, focused);
                }
            }
        }
        None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Tree")
                .border_style(if focused {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                });
            frame.render_widget(
                Paragraph::new("← worktree を選択してください")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
        }
    }
}

/// Tree/Diff/Context モードを切り替える
pub fn toggle_mode(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        wt.right_panel_mode = match wt.right_panel_mode {
            RightPanelMode::Tree => RightPanelMode::Diff,
            RightPanelMode::Diff => RightPanelMode::Context,
            RightPanelMode::Context => RightPanelMode::Tree,
        };
    }
}

/// Context 一覧を描画する
fn render_context_list(
    frame: &mut Frame,
    area: Rect,
    items: &[(String, String)],
    cursor: usize,
    focused: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Context")
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("(no contexts)")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let style = if i == cursor {
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("  {}", name)).style(style)
        })
        .collect();

    let list = List::new(list_items).block(block);
    frame.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SikiConfig, ProjectConfig, WorktreeConfig};

    fn app_with_worktree() -> App {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
                base_branch: None,
            },
            projects: vec![ProjectConfig {
                name: "test".to_string(),
                path: "/tmp/test".to_string(),
                display_name: None,
                worktrees: vec![WorktreeConfig {
                    name: "wt1".to_string(),
                    branch: "main".to_string(),
                }],
            }],
        };
        let mut app = App::new(&config);
        app.selected_worktree = Some((0, 0));
        app
    }

    #[test]
    fn test_toggle_mode() {
        let mut app = app_with_worktree();
        assert_eq!(
            app.selected_worktree().unwrap().right_panel_mode,
            RightPanelMode::Tree
        );

        toggle_mode(&mut app);
        assert_eq!(
            app.selected_worktree().unwrap().right_panel_mode,
            RightPanelMode::Diff
        );

        toggle_mode(&mut app);
        assert_eq!(
            app.selected_worktree().unwrap().right_panel_mode,
            RightPanelMode::Context
        );

        toggle_mode(&mut app);
        assert_eq!(
            app.selected_worktree().unwrap().right_panel_mode,
            RightPanelMode::Tree
        );
    }

    #[test]
    fn test_toggle_mode_no_worktree() {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
                base_branch: None,
            },
            projects: vec![],
        };
        let mut app = App::new(&config);
        // パニックしない
        toggle_mode(&mut app);
    }
}

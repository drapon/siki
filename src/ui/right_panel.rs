use crate::app::{App, DiffFocus, RightPanelMode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

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
            };
            let tabs = Tabs::new(vec!["Tree", "Changes"])
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

                    diff_view.render(frame, diff_chunks[0], pr_focused, Some("PR Changes (●local ↑pushed)"));
                    local_changes.render(frame, diff_chunks[1], local_focused, Some("Uncommitted"));
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

/// Tree/Diff モードを切り替える
pub fn toggle_mode(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        wt.right_panel_mode = match wt.right_panel_mode {
            RightPanelMode::Tree => RightPanelMode::Diff,
            RightPanelMode::Diff => RightPanelMode::Tree,
        };
    }
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

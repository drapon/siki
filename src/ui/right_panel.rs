use crate::app::{App, RightPanelMode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

use super::diff_file_list::DiffFileList;
use super::source_tree::SourceTree;

/// 右パネル上部を描画する
pub fn render_top(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    source_tree: &SourceTree,
    diff_file_list: &DiffFileList,
    focused: bool,
) {
    match app.selected_worktree() {
        Some(wt) => {
            let border_style = if focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // 外枠ブロック
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style);
            let inner = block.inner(area);
            frame.render_widget(block, area);

            // タブバー + コンテンツの分割
            let chunks = Layout::vertical([
                Constraint::Length(1), // タブバー
                Constraint::Min(0),    // コンテンツ
            ])
            .split(inner);

            // タブバー描画
            let titles = vec!["一覧", "差分"];
            let active = match wt.right_panel_mode {
                RightPanelMode::Tree => 0,
                RightPanelMode::Diff => 1,
            };
            let tabs = Tabs::new(titles)
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
                    source_tree.render_content(frame, chunks[1], focused);
                }
                RightPanelMode::Diff => {
                    diff_file_list.render(frame, chunks[1], focused);
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

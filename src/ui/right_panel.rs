use crate::app::{App, RightPanelMode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::diff_view::DiffView;
use super::source_tree::SourceTree;

/// 右パネル上部を描画する
pub fn render_top(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    source_tree: &SourceTree,
    diff_view: &DiffView,
    focused: bool,
) {
    match app.selected_worktree() {
        Some(wt) => match wt.right_panel_mode {
            RightPanelMode::Tree => {
                source_tree.render(frame, area, focused);
            }
            RightPanelMode::Diff => {
                diff_view.render(frame, area, focused);
            }
        },
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
            },
            projects: vec![],
        };
        let mut app = App::new(&config);
        // パニックしない
        toggle_mode(&mut app);
    }
}

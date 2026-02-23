use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::path::Path;
use std::process::Command;

/// Git diff の状態
#[derive(Debug)]
pub struct DiffView {
    pub content: String,
    pub scroll_offset: usize,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            scroll_offset: 0,
        }
    }

    /// worktree パスの git diff を取得する
    pub fn load(&mut self, worktree_path: &Path) {
        self.scroll_offset = 0;
        self.content = get_git_diff(worktree_path);
    }

    /// スクロール下
    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    /// スクロール上
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// 描画
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Diff")
            .border_style(if focused {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        if self.content.is_empty() {
            frame.render_widget(
                Paragraph::new("変更なし")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        let lines: Vec<Line> = self
            .content
            .lines()
            .map(|line| {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("@@") {
                    Style::default().fg(Color::Cyan)
                } else if line.starts_with("diff ") || line.starts_with("index ") {
                    Style::default().fg(Color::Yellow).bold()
                } else {
                    Style::default()
                };
                Line::styled(line, style)
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(block)
            .scroll((self.scroll_offset as u16, 0));

        frame.render_widget(paragraph, area);
    }
}

/// git diff コマンドを実行して結果を返す
fn get_git_diff(worktree_path: &Path) -> String {
    let output = Command::new("git")
        .args(["diff"])
        .current_dir(worktree_path)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        Ok(output) => {
            format!(
                "git diff エラー: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        }
        Err(e) => format!("git コマンドの実行に失敗: {}", e),
    }
}

/// テスト用: diff 文字列を直接パースしてスタイル判定するためのヘルパー
pub fn classify_diff_line(line: &str) -> DiffLineType {
    if line.starts_with('+') && !line.starts_with("+++") {
        DiffLineType::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        DiffLineType::Removed
    } else if line.starts_with("@@") {
        DiffLineType::Hunk
    } else if line.starts_with("diff ") || line.starts_with("index ") {
        DiffLineType::Header
    } else {
        DiffLineType::Context
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineType {
    Added,
    Removed,
    Hunk,
    Header,
    Context,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_added_line() {
        assert_eq!(classify_diff_line("+new line"), DiffLineType::Added);
    }

    #[test]
    fn test_classify_removed_line() {
        assert_eq!(classify_diff_line("-old line"), DiffLineType::Removed);
    }

    #[test]
    fn test_classify_hunk_header() {
        assert_eq!(
            classify_diff_line("@@ -1,3 +1,4 @@"),
            DiffLineType::Hunk
        );
    }

    #[test]
    fn test_classify_diff_header() {
        assert_eq!(
            classify_diff_line("diff --git a/file b/file"),
            DiffLineType::Header
        );
    }

    #[test]
    fn test_classify_index_header() {
        assert_eq!(
            classify_diff_line("index abc123..def456 100644"),
            DiffLineType::Header
        );
    }

    #[test]
    fn test_classify_context_line() {
        assert_eq!(classify_diff_line(" unchanged line"), DiffLineType::Context);
    }

    #[test]
    fn test_classify_triple_plus() {
        // +++ は追加行ではなくコンテキスト扱い
        assert_eq!(classify_diff_line("+++ b/file.rs"), DiffLineType::Context);
    }

    #[test]
    fn test_classify_triple_minus() {
        // --- は削除行ではなくコンテキスト扱い
        assert_eq!(classify_diff_line("--- a/file.rs"), DiffLineType::Context);
    }

    #[test]
    fn test_diff_view_scroll() {
        let mut view = DiffView::new();
        assert_eq!(view.scroll_offset, 0);

        view.scroll_down();
        assert_eq!(view.scroll_offset, 1);
        view.scroll_down();
        assert_eq!(view.scroll_offset, 2);
        view.scroll_up();
        assert_eq!(view.scroll_offset, 1);
        view.scroll_up();
        assert_eq!(view.scroll_offset, 0);
        view.scroll_up(); // 0 以下にはならない
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_diff_view_new() {
        let view = DiffView::new();
        assert!(view.content.is_empty());
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_diff_view_load_nonexistent() {
        let mut view = DiffView::new();
        view.load(Path::new("/nonexistent/path"));
        // エラーメッセージが content に入る
        assert!(!view.content.is_empty());
        assert_eq!(view.scroll_offset, 0);
    }
}

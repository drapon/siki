use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, ListState};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 差分があるファイルの一覧
#[derive(Debug)]
pub struct DiffFileList {
    pub files: Vec<String>,
    pub cursor_index: usize,
    pub worktree_path: PathBuf,
}

impl DiffFileList {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            cursor_index: 0,
            worktree_path: PathBuf::new(),
        }
    }

    /// worktree パスの変更ファイル一覧を取得する
    pub fn load(&mut self, worktree_path: &Path) {
        self.worktree_path = worktree_path.to_path_buf();
        self.cursor_index = 0;
        self.files = get_diff_files(worktree_path);
    }

    /// カーソルを上に移動
    pub fn move_up(&mut self) {
        if self.cursor_index > 0 {
            self.cursor_index -= 1;
        }
    }

    /// カーソルを下に移動
    pub fn move_down(&mut self) {
        if !self.files.is_empty() && self.cursor_index < self.files.len() - 1 {
            self.cursor_index += 1;
        }
    }

    /// 現在選択中のファイルのフルパスを返す
    pub fn current_file_path(&self) -> Option<PathBuf> {
        self.files
            .get(self.cursor_index)
            .map(|f| self.worktree_path.join(f))
    }

    /// ボーダーなしで描画（right_panel が外枠を管理）
    pub fn render(&self, frame: &mut Frame, area: Rect, _focused: bool) {
        if self.files.is_empty() {
            let text = Text::from("変更なし").style(Style::default().fg(Color::DarkGray));
            frame.render_widget(text, area);
            return;
        }

        let items: Vec<ListItem> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let style = if i == self.cursor_index {
                    Style::default().fg(Color::White).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                ListItem::new(format!("  {}", file)).style(style)
            })
            .collect();

        let list = List::new(items);
        let mut list_state = ListState::default();
        list_state.select(Some(self.cursor_index));
        frame.render_stateful_widget(list, area, &mut list_state);
    }
}

/// git diff --name-only を実行して変更ファイル一覧を返す
fn get_diff_files(worktree_path: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(worktree_path)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let list = DiffFileList::new();
        assert!(list.files.is_empty());
        assert_eq!(list.cursor_index, 0);
    }

    #[test]
    fn test_move_up_down() {
        let mut list = DiffFileList::new();
        list.files = vec![
            "file1.rs".to_string(),
            "file2.rs".to_string(),
            "file3.rs".to_string(),
        ];

        assert_eq!(list.cursor_index, 0);
        list.move_down();
        assert_eq!(list.cursor_index, 1);
        list.move_down();
        assert_eq!(list.cursor_index, 2);
        list.move_down(); // 末尾で止まる
        assert_eq!(list.cursor_index, 2);

        list.move_up();
        assert_eq!(list.cursor_index, 1);
        list.move_up();
        assert_eq!(list.cursor_index, 0);
        list.move_up(); // 先頭で止まる
        assert_eq!(list.cursor_index, 0);
    }

    #[test]
    fn test_move_empty() {
        let mut list = DiffFileList::new();
        list.move_down();
        assert_eq!(list.cursor_index, 0);
        list.move_up();
        assert_eq!(list.cursor_index, 0);
    }

    #[test]
    fn test_current_file_path() {
        let mut list = DiffFileList::new();
        list.worktree_path = PathBuf::from("/tmp/project");
        list.files = vec!["src/main.rs".to_string()];

        assert_eq!(
            list.current_file_path(),
            Some(PathBuf::from("/tmp/project/src/main.rs"))
        );
    }

    #[test]
    fn test_current_file_path_empty() {
        let list = DiffFileList::new();
        assert_eq!(list.current_file_path(), None);
    }

    #[test]
    fn test_load_nonexistent() {
        let mut list = DiffFileList::new();
        list.load(Path::new("/nonexistent/path"));
        assert!(list.files.is_empty());
        assert_eq!(list.cursor_index, 0);
    }
}

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use std::path::{Path, PathBuf};

/// ツリーの各エントリ
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// ソースツリーの状態
#[derive(Debug)]
pub struct SourceTree {
    pub entries: Vec<TreeEntry>,
    pub cursor_index: usize,
    pub scroll_offset: usize,
    /// 検索モード中か
    pub search_active: bool,
    /// 検索文字列
    pub search_query: String,
    /// マッチしたエントリインデックス
    pub search_matches: Vec<usize>,
    /// 現在のマッチ位置
    pub search_match_idx: usize,
}

impl SourceTree {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor_index: 0,
            scroll_offset: 0,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        }
    }

    /// 指定ディレクトリからツリーを構築する
    pub fn load(&mut self, root: &Path) {
        self.entries.clear();
        self.cursor_index = 0;
        self.scroll_offset = 0;
        if root.is_dir() {
            self.scan_dir(root, 0);
        }
    }

    /// ディレクトリを再帰的に走査
    fn scan_dir(&mut self, dir: &Path, depth: usize) {
        let mut children: Vec<(bool, String, PathBuf)> = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                // 隠しファイル・ディレクトリをスキップ
                if name.starts_with('.') {
                    continue;
                }

                let is_dir = path.is_dir();
                children.push((is_dir, name, path));
            }
        }

        // ディレクトリ優先、その後名前順
        children.sort_by(|a, b| {
            match (a.0, b.0) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
            }
        });

        for (is_dir, name, path) in children {
            self.entries.push(TreeEntry {
                path: path.clone(),
                name,
                depth,
                is_dir,
                expanded: false,
            });
        }
    }

    /// カーソルを下に移動
    pub fn move_down(&mut self) {
        let visible = self.visible_entries();
        if !visible.is_empty() && self.cursor_index < visible.len() - 1 {
            self.cursor_index += 1;
        }
    }

    /// カーソルを上に移動
    pub fn move_up(&mut self) {
        if self.cursor_index > 0 {
            self.cursor_index -= 1;
        }
    }

    /// 現在カーソルのディレクトリを展開
    pub fn expand(&mut self) {
        let visible = self.visible_entries();
        if let Some(idx) = visible.get(self.cursor_index).copied() {
            let entry = &self.entries[idx];
            if entry.is_dir && !entry.expanded {
                let path = entry.path.clone();
                let depth = entry.depth + 1;
                self.entries[idx].expanded = true;

                // 子エントリを挿入
                let children = Self::scan_children(&path, depth);
                let insert_pos = idx + 1;
                for (i, child) in children.into_iter().enumerate() {
                    self.entries.insert(insert_pos + i, child);
                }
            }
        }
    }

    /// 現在カーソルのディレクトリを折りたたむ
    pub fn collapse(&mut self) {
        let visible = self.visible_entries();
        if let Some(idx) = visible.get(self.cursor_index).copied() {
            let entry = &self.entries[idx];
            if entry.is_dir && entry.expanded {
                let depth = entry.depth;
                self.entries[idx].expanded = false;

                // 子エントリを削除
                let mut remove_count = 0;
                for i in (idx + 1)..self.entries.len() {
                    if self.entries[i].depth > depth {
                        remove_count += 1;
                    } else {
                        break;
                    }
                }
                self.entries.drain((idx + 1)..(idx + 1 + remove_count));
            }
        }
    }

    /// 現在カーソルがディレクトリかどうか
    pub fn current_is_dir(&self) -> bool {
        let visible = self.visible_entries();
        visible
            .get(self.cursor_index)
            .and_then(|&idx| self.entries.get(idx))
            .map(|e| e.is_dir)
            .unwrap_or(false)
    }

    /// 現在カーソルのディレクトリを展開/折りたたみトグル
    pub fn toggle(&mut self) {
        let visible = self.visible_entries();
        if let Some(&idx) = visible.get(self.cursor_index) {
            if self.entries[idx].is_dir {
                if self.entries[idx].expanded {
                    self.collapse();
                } else {
                    self.expand();
                }
            }
        }
    }

    /// 現在カーソルのファイルパスを取得（ファイルの場合のみ）
    pub fn current_file_path(&self) -> Option<PathBuf> {
        let visible = self.visible_entries();
        let idx = visible.get(self.cursor_index).copied()?;
        let entry = &self.entries[idx];
        if !entry.is_dir {
            Some(entry.path.clone())
        } else {
            None
        }
    }

    /// 検索モード開始
    pub fn search_start(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
    }

    /// 検索キャンセル
    pub fn search_cancel(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
    }

    /// 検索確定（マッチ結果は保持して検索モードを終了）
    pub fn search_confirm(&mut self) {
        self.search_active = false;
    }

    /// 検索文字を追加
    pub fn search_push(&mut self, c: char) {
        self.search_query.push(c);
        self.update_matches();
    }

    /// 検索文字を削除
    pub fn search_pop(&mut self) {
        self.search_query.pop();
        self.update_matches();
    }

    /// entries を走査してマッチ一覧を更新、最初のマッチにカーソル移動
    fn update_matches(&mut self) {
        self.search_matches.clear();
        self.search_match_idx = 0;

        if self.search_query.is_empty() {
            return;
        }

        let query_lower = self.search_query.to_lowercase();
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.name.to_lowercase().contains(&query_lower) {
                self.search_matches.push(i);
            }
        }

        if let Some(&first) = self.search_matches.first() {
            self.cursor_index = first;
        }
    }

    /// 次のマッチへ移動
    pub fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.cursor_index = self.search_matches[self.search_match_idx];
    }

    /// 前のマッチへ移動
    pub fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        if self.search_match_idx == 0 {
            self.search_match_idx = self.search_matches.len() - 1;
        } else {
            self.search_match_idx -= 1;
        }
        self.cursor_index = self.search_matches[self.search_match_idx];
    }

    /// 表示すべきエントリのインデックス一覧を返す
    /// （全エントリが表示対象。展開/折りたたみは entries の追加/削除で管理）
    fn visible_entries(&self) -> Vec<usize> {
        (0..self.entries.len()).collect()
    }

    /// ディレクトリの子エントリを走査して返す
    fn scan_children(dir: &Path, depth: usize) -> Vec<TreeEntry> {
        let mut children: Vec<(bool, String, PathBuf)> = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = path.is_dir();
                children.push((is_dir, name, path));
            }
        }

        children.sort_by(|a, b| {
            match (a.0, b.0) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
            }
        });

        children
            .into_iter()
            .map(|(is_dir, name, path)| TreeEntry {
                path,
                name,
                depth,
                is_dir,
                expanded: false,
            })
            .collect()
    }

    /// 検索中に表示するフィルタ済みエントリのインデックスを返す
    fn filtered_entries(&self) -> Vec<usize> {
        if self.search_active && !self.search_query.is_empty() {
            self.search_matches.clone()
        } else {
            (0..self.entries.len()).collect()
        }
    }

    /// 描画
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        if self.search_active {
            // 検索モード: 上部に検索入力、下部にフィルタ済みリスト
            let chunks = Layout::vertical([
                Constraint::Length(1), // 検索入力行
                Constraint::Min(0),    // リスト
            ])
            .split(Block::default().borders(Borders::ALL).inner(area));

            // 外枠ブロック
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Tree")
                .border_style(border_style);
            frame.render_widget(block, area);

            // 検索入力行
            let search_text = format!("/{}_", self.search_query);
            let match_info = if self.search_query.is_empty() {
                String::new()
            } else {
                format!(" [{} found]", self.search_matches.len())
            };
            let search_line = Line::from(vec![
                Span::styled(search_text, Style::default().fg(Color::Cyan)),
                Span::styled(match_info, Style::default().fg(Color::DarkGray)),
            ]);
            frame.render_widget(Paragraph::new(search_line), chunks[0]);

            // フィルタ済みリスト
            let filtered = self.filtered_entries();
            let items: Vec<ListItem> = filtered
                .iter()
                .map(|&i| {
                    let entry = &self.entries[i];
                    let indent = "  ".repeat(entry.depth);
                    let icon = if entry.is_dir {
                        if entry.expanded { "▾ " } else { "▸ " }
                    } else {
                        "  "
                    };
                    let style = if i == self.cursor_index {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    } else if entry.is_dir {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!("{}{}{}", indent, icon, entry.name)).style(style)
                })
                .collect();

            let selected_pos = filtered
                .iter()
                .position(|&i| i == self.cursor_index);

            let list = List::new(items);
            let mut list_state = ListState::default();
            list_state.select(selected_pos);
            frame.render_stateful_widget(list, chunks[1], &mut list_state);
        } else {
            // 通常モード
            let items: Vec<ListItem> = self
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let indent = "  ".repeat(entry.depth);
                    let icon = if entry.is_dir {
                        if entry.expanded { "▾ " } else { "▸ " }
                    } else {
                        "  "
                    };

                    let is_match = !self.search_matches.is_empty()
                        && self.search_matches.contains(&i);
                    let style = if i == self.cursor_index {
                        if entry.is_dir {
                            Style::default().fg(Color::Yellow).bg(Color::DarkGray)
                        } else {
                            Style::default().fg(Color::White).bg(Color::DarkGray)
                        }
                    } else if is_match {
                        Style::default().bg(Color::Yellow).fg(Color::Black)
                    } else if entry.is_dir {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };

                    ListItem::new(format!("{}{}{}", indent, icon, entry.name)).style(style)
                })
                .collect();

            let block = Block::default()
                .borders(Borders::ALL)
                .title("Tree")
                .border_style(border_style);

            let list = List::new(items).block(block);
            let mut list_state = ListState::default();
            list_state.select(Some(self.cursor_index));

            frame.render_stateful_widget(list, area, &mut list_state);
        }
    }

    /// ボーダーなしで描画（right_panel が外枠を管理）
    pub fn render_content(&self, frame: &mut Frame, area: Rect, _focused: bool) {
        if self.search_active {
            let chunks = Layout::vertical([
                Constraint::Length(1), // 検索入力行
                Constraint::Min(0),    // リスト
            ])
            .split(area);

            // 検索入力行
            let search_text = format!("/{}_", self.search_query);
            let match_info = if self.search_query.is_empty() {
                String::new()
            } else {
                format!(" [{} found]", self.search_matches.len())
            };
            let search_line = Line::from(vec![
                Span::styled(search_text, Style::default().fg(Color::Cyan)),
                Span::styled(match_info, Style::default().fg(Color::DarkGray)),
            ]);
            frame.render_widget(Paragraph::new(search_line), chunks[0]);

            // フィルタ済みリスト
            let filtered = self.filtered_entries();
            let items: Vec<ListItem> = filtered
                .iter()
                .map(|&i| {
                    let entry = &self.entries[i];
                    let indent = "  ".repeat(entry.depth);
                    let icon = if entry.is_dir {
                        if entry.expanded { "▾ " } else { "▸ " }
                    } else {
                        "  "
                    };
                    let style = if i == self.cursor_index {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    } else if entry.is_dir {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!("{}{}{}", indent, icon, entry.name)).style(style)
                })
                .collect();

            let selected_pos = filtered.iter().position(|&i| i == self.cursor_index);
            let list = List::new(items);
            let mut list_state = ListState::default();
            list_state.select(selected_pos);
            frame.render_stateful_widget(list, chunks[1], &mut list_state);
        } else {
            let items: Vec<ListItem> = self
                .entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let indent = "  ".repeat(entry.depth);
                    let icon = if entry.is_dir {
                        if entry.expanded { "▾ " } else { "▸ " }
                    } else {
                        "  "
                    };

                    let is_match = !self.search_matches.is_empty()
                        && self.search_matches.contains(&i);
                    let style = if i == self.cursor_index {
                        if entry.is_dir {
                            Style::default().fg(Color::Yellow).bg(Color::DarkGray)
                        } else {
                            Style::default().fg(Color::White).bg(Color::DarkGray)
                        }
                    } else if is_match {
                        Style::default().bg(Color::Yellow).fg(Color::Black)
                    } else if entry.is_dir {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };

                    ListItem::new(format!("{}{}{}", indent, icon, entry.name)).style(style)
                })
                .collect();

            let list = List::new(items);
            let mut list_state = ListState::default();
            list_state.select(Some(self.cursor_index));
            frame.render_stateful_widget(list, area, &mut list_state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // ディレクトリ構造:
        // root/
        //   src/
        //     main.rs
        //     lib.rs
        //   README.md
        //   Cargo.toml
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/lib.rs"), "pub mod foo;").unwrap();
        fs::write(root.join("README.md"), "# Hello").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();

        dir
    }

    #[test]
    fn test_load_tree() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // src ディレクトリが最初（ディレクトリ優先）
        assert!(!tree.entries.is_empty());
        assert!(tree.entries[0].is_dir);
        assert_eq!(tree.entries[0].name, "src");

        // ファイルが後に来る
        let file_names: Vec<&str> = tree
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.as_str())
            .collect();
        assert!(file_names.contains(&"Cargo.toml"));
        assert!(file_names.contains(&"README.md"));
    }

    #[test]
    fn test_load_skips_hidden() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".hidden"), "secret").unwrap();
        fs::write(dir.path().join("visible.txt"), "hello").unwrap();

        let mut tree = SourceTree::new();
        tree.load(dir.path());

        assert_eq!(tree.entries.len(), 1);
        assert_eq!(tree.entries[0].name, "visible.txt");
    }

    #[test]
    fn test_load_depth_zero() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // 初期ロードは depth=0 のみ（ルート直下）
        for entry in &tree.entries {
            assert_eq!(entry.depth, 0);
        }
    }

    #[test]
    fn test_move_down_up() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        assert_eq!(tree.cursor_index, 0);
        tree.move_down();
        assert_eq!(tree.cursor_index, 1);
        tree.move_down();
        assert_eq!(tree.cursor_index, 2);
        tree.move_up();
        assert_eq!(tree.cursor_index, 1);
        tree.move_up();
        assert_eq!(tree.cursor_index, 0);
    }

    #[test]
    fn test_move_up_at_top() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        tree.move_up();
        assert_eq!(tree.cursor_index, 0);
    }

    #[test]
    fn test_move_down_at_bottom() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        let len = tree.entries.len();
        for _ in 0..len + 5 {
            tree.move_down();
        }
        assert_eq!(tree.cursor_index, len - 1);
    }

    #[test]
    fn test_expand_directory() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // src は最初のエントリ（ディレクトリ優先）
        assert_eq!(tree.entries[0].name, "src");
        assert!(!tree.entries[0].expanded);

        tree.expand();

        assert!(tree.entries[0].expanded);
        // src の子が挿入される
        assert!(tree.entries.len() > 3); // src + children + Cargo.toml + README.md
        assert_eq!(tree.entries[1].depth, 1);
        assert!(
            tree.entries[1].name == "lib.rs" || tree.entries[1].name == "main.rs"
        );
    }

    #[test]
    fn test_collapse_directory() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        let original_len = tree.entries.len();

        tree.expand(); // src を展開
        assert!(tree.entries.len() > original_len);

        tree.collapse(); // src を折りたたみ
        assert_eq!(tree.entries.len(), original_len);
        assert!(!tree.entries[0].expanded);
    }

    #[test]
    fn test_expand_file_does_nothing() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // ファイル行にカーソルを移動
        let file_idx = tree
            .entries
            .iter()
            .position(|e| !e.is_dir)
            .unwrap();
        tree.cursor_index = file_idx;

        let len_before = tree.entries.len();
        tree.expand();
        assert_eq!(tree.entries.len(), len_before);
    }

    #[test]
    fn test_current_file_path_on_file() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        let file_idx = tree
            .entries
            .iter()
            .position(|e| !e.is_dir)
            .unwrap();
        tree.cursor_index = file_idx;

        let path = tree.current_file_path();
        assert!(path.is_some());
    }

    #[test]
    fn test_current_file_path_on_dir() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // src ディレクトリにカーソル
        assert!(tree.entries[0].is_dir);
        tree.cursor_index = 0;

        let path = tree.current_file_path();
        assert!(path.is_none());
    }

    #[test]
    fn test_empty_directory() {
        let dir = TempDir::new().unwrap();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        assert!(tree.entries.is_empty());
        // move は何もしない
        tree.move_down();
        tree.move_up();
        assert_eq!(tree.cursor_index, 0);
    }

    #[test]
    fn test_nonexistent_path() {
        let mut tree = SourceTree::new();
        tree.load(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(tree.entries.is_empty());
    }

    // --- 検索機能のテスト ---

    fn create_search_tree() -> SourceTree {
        let mut tree = SourceTree::new();
        tree.entries = vec![
            TreeEntry { path: "src".into(), name: "src".to_string(), depth: 0, is_dir: true, expanded: false },
            TreeEntry { path: "Cargo.toml".into(), name: "Cargo.toml".to_string(), depth: 0, is_dir: false, expanded: false },
            TreeEntry { path: "README.md".into(), name: "README.md".to_string(), depth: 0, is_dir: false, expanded: false },
            TreeEntry { path: "cargo.lock".into(), name: "cargo.lock".to_string(), depth: 0, is_dir: false, expanded: false },
        ];
        tree
    }

    #[test]
    fn test_search_name_match() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('R');
        tree.search_push('E');
        tree.search_push('A');

        assert_eq!(tree.search_matches.len(), 1);
        assert_eq!(tree.cursor_index, 2); // README.md
    }

    #[test]
    fn test_search_case_insensitive() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('c');
        tree.search_push('a');
        tree.search_push('r');
        tree.search_push('g');
        tree.search_push('o');

        // "Cargo.toml" と "cargo.lock" の両方にマッチ
        assert_eq!(tree.search_matches.len(), 2);
        assert!(tree.search_matches.contains(&1)); // Cargo.toml
        assert!(tree.search_matches.contains(&3)); // cargo.lock
    }

    #[test]
    fn test_search_next_prev_wraps() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('c');
        tree.search_push('a');
        tree.search_push('r');
        tree.search_push('g');
        tree.search_push('o');
        tree.search_confirm();

        // 最初のマッチ
        assert_eq!(tree.cursor_index, 1); // Cargo.toml

        // 次のマッチ
        tree.next_match();
        assert_eq!(tree.cursor_index, 3); // cargo.lock

        // さらに次で循環
        tree.next_match();
        assert_eq!(tree.cursor_index, 1); // Cargo.toml に戻る

        // 前のマッチ（循環）
        tree.prev_match();
        assert_eq!(tree.cursor_index, 3); // cargo.lock
    }

    #[test]
    fn test_search_no_match_cursor_stays() {
        let mut tree = create_search_tree();
        tree.cursor_index = 2;
        let original_cursor = tree.cursor_index;

        tree.search_start();
        tree.search_push('z');
        tree.search_push('z');
        tree.search_push('z');

        assert!(tree.search_matches.is_empty());
        assert_eq!(tree.cursor_index, original_cursor);
    }

    #[test]
    fn test_search_cancel_clears_state() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('c');
        assert!(tree.search_active);
        assert!(!tree.search_query.is_empty());

        tree.search_cancel();
        assert!(!tree.search_active);
        assert!(tree.search_query.is_empty());
        assert!(tree.search_matches.is_empty());
    }

    #[test]
    fn test_search_confirm_keeps_matches() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('c');
        tree.search_push('a');
        tree.search_push('r');
        tree.search_push('g');
        tree.search_push('o');
        let match_count = tree.search_matches.len();

        tree.search_confirm();
        assert!(!tree.search_active);
        assert_eq!(tree.search_matches.len(), match_count);
    }

    #[test]
    fn test_search_pop_updates_matches() {
        let mut tree = create_search_tree();
        tree.search_start();
        tree.search_push('c');
        tree.search_push('a');
        tree.search_push('r');
        tree.search_push('g');
        tree.search_push('o');
        assert_eq!(tree.search_matches.len(), 2);

        // "cargo" → "carg" → "car" ... 文字削除でマッチ更新
        tree.search_pop(); // "carg"
        assert_eq!(tree.search_matches.len(), 2); // まだ2つマッチ

        // 全部消す
        tree.search_pop(); // "car"
        tree.search_pop(); // "ca"
        tree.search_pop(); // "c"
        tree.search_pop(); // ""
        assert!(tree.search_matches.is_empty()); // 空文字はマッチなし
    }

    #[test]
    fn test_next_prev_match_empty() {
        let mut tree = create_search_tree();
        tree.cursor_index = 1;
        // マッチなしで next/prev は何もしない
        tree.next_match();
        assert_eq!(tree.cursor_index, 1);
        tree.prev_match();
        assert_eq!(tree.cursor_index, 1);
    }

    #[test]
    fn test_sort_order_dirs_first() {
        let dir = create_test_tree();
        let mut tree = SourceTree::new();
        tree.load(dir.path());

        // ディレクトリが先に来ることを確認
        let mut seen_file = false;
        for entry in &tree.entries {
            if !entry.is_dir {
                seen_file = true;
            }
            if entry.is_dir && seen_file {
                panic!("ディレクトリがファイルの後に来ています");
            }
        }
    }
}

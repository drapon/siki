use crate::app::{App, OpenFile};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

/// 中央パネルの描画
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    focused: bool,
    claude_screen: Option<&vt100::Screen>,
) {
    let wt = match app.selected_worktree_mut() {
        Some(wt) => wt,
        None => {
            // worktree 未選択時
            let block = panel_block("Main", focused);
            frame.render_widget(
                Paragraph::new("← プロジェクトから worktree を選択してください")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }
    };

    // タブバー + コンテンツの分割
    let chunks = Layout::vertical([
        Constraint::Length(2), // タブバー（1行 + 下線）
        Constraint::Min(0),   // コンテンツ
    ])
    .split(area);

    // タブバー描画
    render_tab_bar(frame, chunks[0], wt.active_tab, wt.claude_tabs, &wt.open_files, focused);

    // コンテンツ描画
    if wt.active_tab < wt.claude_tabs {
        // Claude タブ
        if let Some(screen) = claude_screen {
            render_claude_terminal(frame, chunks[1], screen, focused);
        } else {
            let block = panel_block("Claude Code", focused);
            frame.render_widget(
                Paragraph::new("起動中...")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                chunks[1],
            );
        }
    } else {
        let file_index = wt.active_tab - wt.claude_tabs;
        if let Some(file) = wt.open_files.get_mut(file_index) {
            render_file(frame, chunks[1], file, focused);
        }
    }
}

/// タブバーを描画
fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    active_tab: usize,
    claude_tabs: usize,
    open_files: &[OpenFile],
    focused: bool,
) {
    let mut titles: Vec<String> = (0..claude_tabs)
        .map(|i| {
            if claude_tabs == 1 {
                "Claude".to_string()
            } else {
                format!("Claude {}", i + 1)
            }
        })
        .collect();
    for f in open_files {
        let name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".to_string());
        titles.push(name);
    }

    let tabs = Tabs::new(titles)
        .select(active_tab)
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

    frame.render_widget(tabs, area);
}

/// Claude Code ターミナルを描画
fn render_claude_terminal(
    frame: &mut Frame,
    area: Rect,
    screen: &vt100::Screen,
    focused: bool,
) {
    let title = if focused {
        "Claude Code (Ctrl+\\ exit)"
    } else {
        "Claude Code"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        });
    let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);
    frame.render_widget(pseudo_term, area);
}

/// ファイル内容をシンタックスハイライト付きで描画（キャッシュ済みデータを使用）
fn render_file(frame: &mut Frame, area: Rect, file: &mut OpenFile, focused: bool) {
    let filename = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "?".to_string());

    // カーソル行の表示（1-indexed）
    let title = format!("{} L{} (read-only)", filename, file.cursor_line + 1);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    // 検索バー用に1行確保（search_active または search_matches が残っている場合）
    let has_search_bar = file.search_active || !file.search_matches.is_empty();
    let content_area = if has_search_bar {
        let chunks = Layout::vertical([
            Constraint::Length(1), // 検索バー
            Constraint::Min(0),   // ファイル内容
        ])
        .split(block.inner(area));
        let search_area = chunks[0];

        // 検索バーを描画
        let search_text = if file.search_active {
            if file.search_matches.is_empty() && !file.search_query.is_empty() {
                format!("/{}_  [no match]", file.search_query)
            } else if !file.search_matches.is_empty() {
                format!(
                    "/{}_  [{}/{}]",
                    file.search_query,
                    file.search_match_idx + 1,
                    file.search_matches.len()
                )
            } else {
                format!("/{}_ ", file.search_query)
            }
        } else {
            format!(
                "/{} [{}/{}]  (n/N: next/prev, /: new search)",
                file.search_query,
                file.search_match_idx + 1,
                file.search_matches.len()
            )
        };
        frame.render_widget(
            Paragraph::new(search_text).style(Style::default().fg(Color::Yellow)),
            search_area,
        );

        chunks[1]
    } else {
        block.inner(area)
    };

    // ボーダーを先に描画
    frame.render_widget(block, area);

    // ビューポート内にカーソルが収まるよう自動スクロール
    let visible_lines = content_area.height as usize;
    if visible_lines > 0 {
        if file.cursor_line < file.scroll_offset {
            file.scroll_offset = file.cursor_line;
        } else if file.cursor_line >= file.scroll_offset + visible_lines {
            file.scroll_offset = file.cursor_line - visible_lines + 1;
        }
    }

    // マッチ行のセットを作成
    let match_set: std::collections::HashSet<usize> =
        file.search_matches.iter().copied().collect();
    let current_match_line = file
        .search_matches
        .get(file.search_match_idx)
        .copied();

    let lines: Vec<Line> = file
        .highlighted
        .iter()
        .enumerate()
        .map(|(i, spans)| {
            let is_cursor_line = i == file.cursor_line;
            let is_current_match = current_match_line == Some(i);
            let is_other_match = !is_current_match && match_set.contains(&i);

            let line_num_style = if is_cursor_line {
                Style::default().fg(Color::Yellow)
            } else if is_current_match {
                Style::default().fg(Color::Yellow).bg(Color::Rgb(80, 60, 0))
            } else if is_other_match {
                Style::default().fg(Color::DarkGray).bg(Color::Rgb(40, 35, 0))
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mut line_spans = vec![Span::styled(
                format!("{:4} ", i + 1),
                line_num_style,
            )];
            for (r, g, b, text) in spans {
                let mut style = Style::default().fg(Color::Rgb(*r, *g, *b));
                if is_cursor_line {
                    style = style.bg(Color::Rgb(40, 40, 50));
                } else if is_current_match {
                    style = style.bg(Color::Rgb(80, 60, 0));
                } else if is_other_match {
                    style = style.bg(Color::Rgb(40, 35, 0));
                }
                line_spans.push(Span::styled(text.clone(), style));
            }
            Line::from(line_spans)
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .scroll((file.scroll_offset as u16, 0));

    frame.render_widget(paragraph, content_area);
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
}

/// タブ切替のロジック
pub fn next_tab(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        let tab_count = wt.claude_tabs + wt.open_files.len();
        if tab_count > 0 {
            wt.active_tab = (wt.active_tab + 1) % tab_count;
        }
    }
}

/// ファイルタブを閉じる（Claude タブは Ctrl+\ で閉じる）
pub fn close_current_tab(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        if wt.active_tab < wt.claude_tabs {
            return; // Claude タブはここでは閉じない
        }
        let file_index = wt.active_tab - wt.claude_tabs;
        if file_index < wt.open_files.len() {
            wt.open_files.remove(file_index);
            let tab_count = wt.claude_tabs + wt.open_files.len();
            if tab_count == 0 {
                wt.active_tab = 0;
            } else if wt.active_tab >= tab_count {
                wt.active_tab = tab_count - 1;
            }
        }
    }
}

/// ファイルを新しいタブで開く（既に開いていればそのタブに切替）
pub fn open_file_tab(app: &mut App, path: std::path::PathBuf) {
    if let Some(wt) = app.selected_worktree_mut() {
        // 既に開いているか確認
        for (i, f) in wt.open_files.iter().enumerate() {
            if f.path == path {
                wt.active_tab = wt.claude_tabs + i;
                return;
            }
        }
        // ファイル読み込み
        let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            format!("ファイルを読み込めません: {}", e)
        });
        let highlighted = super::syntax::highlight(&content, &path);
        wt.open_files.push(OpenFile {
            path,
            content,
            scroll_offset: 0,
            cursor_line: 0,
            highlighted,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = wt.claude_tabs + wt.open_files.len() - 1;
    }
}

/// カーソルを下に移動（Claude タブはターミナルが処理するので対象外）
pub fn scroll_down(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        if wt.active_tab < wt.claude_tabs {
            return;
        }
        let file_index = wt.active_tab - wt.claude_tabs;
        if let Some(file) = wt.open_files.get_mut(file_index) {
            let total_lines = file.highlighted.len();
            if total_lines > 0 && file.cursor_line < total_lines - 1 {
                file.cursor_line += 1;
            }
        }
    }
}

/// カーソルを上に移動（Claude タブはターミナルが処理するので対象外）
pub fn scroll_up(app: &mut App) {
    if let Some(wt) = app.selected_worktree_mut() {
        if wt.active_tab < wt.claude_tabs {
            return;
        }
        let file_index = wt.active_tab - wt.claude_tabs;
        if let Some(file) = wt.open_files.get_mut(file_index) {
            file.cursor_line = file.cursor_line.saturating_sub(1);
        }
    }
}

/// ファイル内検索モードが有効か
pub fn is_file_search_active(app: &App) -> bool {
    let wt = match app.selected_worktree() {
        Some(wt) => wt,
        None => return false,
    };
    if wt.active_tab < wt.claude_tabs {
        return false;
    }
    let file_index = wt.active_tab - wt.claude_tabs;
    wt.open_files
        .get(file_index)
        .map(|f| f.search_active)
        .unwrap_or(false)
}

/// 現在のファイルに対して検索操作を行うヘルパー
fn with_current_file_mut(app: &mut App, f: impl FnOnce(&mut OpenFile)) {
    if let Some(wt) = app.selected_worktree_mut() {
        if wt.active_tab >= wt.claude_tabs {
            let file_index = wt.active_tab - wt.claude_tabs;
            if let Some(file) = wt.open_files.get_mut(file_index) {
                f(file);
            }
        }
    }
}

pub fn file_search_start(app: &mut App) {
    with_current_file_mut(app, |f| f.search_start());
}

pub fn file_search_cancel(app: &mut App) {
    with_current_file_mut(app, |f| f.search_cancel());
}

pub fn file_search_confirm(app: &mut App) {
    with_current_file_mut(app, |f| f.search_confirm());
}

pub fn file_search_push(app: &mut App, c: char) {
    with_current_file_mut(app, |f| f.search_push(c));
}

pub fn file_search_pop(app: &mut App) {
    with_current_file_mut(app, |f| f.search_pop());
}

pub fn file_next_match(app: &mut App) {
    with_current_file_mut(app, |f| f.next_match());
}

pub fn file_prev_match(app: &mut App) {
    with_current_file_mut(app, |f| f.prev_match());
}

/// 現在開いているファイルのパス:行番号を返す（1-indexed）
pub fn current_file_location(app: &App) -> Option<String> {
    let wt = app.selected_worktree()?;
    if wt.active_tab < wt.claude_tabs {
        return None;
    }
    let file_index = wt.active_tab - wt.claude_tabs;
    let file = wt.open_files.get(file_index)?;
    Some(format!("{}:{}", file.path.display(), file.cursor_line + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SikiConfig, ProjectConfig, WorktreeConfig};
    use std::path::PathBuf;

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
    fn test_next_tab_no_tabs() {
        let mut app = app_with_worktree();
        // claude_tabs=0, ファイルなし → タブ切替しても 0 のまま
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_next_tab_cycles_with_claude_and_files() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "fn main() {}".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/lib.rs"),
            content: "pub mod foo;".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });

        // Claude(0) → test.rs(1) → lib.rs(2) → Claude(0)
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 1);
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 2);
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_next_tab_files_only() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/a.rs"),
            content: "a".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/b.rs"),
            content: "b".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });

        // a.rs(0) → b.rs(1) → a.rs(0)
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 1);
        next_tab(&mut app);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_close_claude_tab_not_allowed() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.active_tab = 0; // Claude タブ
        close_current_tab(&mut app);
        // Claude タブはここでは閉じない
        assert_eq!(app.selected_worktree().unwrap().claude_tabs, 1);
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_close_file_tab() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "fn main() {}".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 0; // claude_tabs=0 なのでファイルタブ

        close_current_tab(&mut app);
        assert!(app.selected_worktree().unwrap().open_files.is_empty());
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_close_file_tab_with_claude() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "fn main() {}".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 1; // ファイルタブ

        close_current_tab(&mut app);
        assert!(app.selected_worktree().unwrap().open_files.is_empty());
        assert_eq!(app.selected_worktree().unwrap().active_tab, 0);
    }

    #[test]
    fn test_close_middle_tab() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/a.rs"),
            content: "a".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/b.rs"),
            content: "b".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/c.rs"),
            content: "c".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 2; // b.rs (claude=1, a=1, b=2, c=3)

        close_current_tab(&mut app);
        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.open_files.len(), 2);
        assert_eq!(wt.open_files[0].path, PathBuf::from("/tmp/a.rs"));
        assert_eq!(wt.open_files[1].path, PathBuf::from("/tmp/c.rs"));
        assert_eq!(wt.active_tab, 2); // c.rs に移動
    }

    #[test]
    fn test_close_last_file_tab() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/a.rs"),
            content: "a".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/b.rs"),
            content: "b".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 2; // b.rs (last file tab)

        close_current_tab(&mut app);
        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.open_files.len(), 1);
        assert_eq!(wt.active_tab, 1); // a.rs にフォールバック
    }

    #[test]
    fn test_open_file_tab_new() {
        let mut app = app_with_worktree();
        open_file_tab(&mut app, PathBuf::from("/nonexistent/test.rs"));

        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.open_files.len(), 1);
        assert_eq!(wt.active_tab, 0); // claude_tabs=0 なので file[0] = tab 0
        assert!(wt.open_files[0].content.contains("読み込めません"));
    }

    #[test]
    fn test_open_file_tab_new_with_claude() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        open_file_tab(&mut app, PathBuf::from("/nonexistent/test.rs"));

        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.open_files.len(), 1);
        assert_eq!(wt.active_tab, 1); // claude_tabs=1 なので file[0] = tab 1
    }

    #[test]
    fn test_open_file_tab_already_open() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/existing.rs"),
            content: "existing".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 0; // Claude タブ

        open_file_tab(&mut app, PathBuf::from("/tmp/existing.rs"));

        let wt = app.selected_worktree().unwrap();
        assert_eq!(wt.open_files.len(), 1); // 増えない
        assert_eq!(wt.active_tab, 1); // 既存ファイルタブに切替
    }

    #[test]
    fn test_scroll_down_claude_tab_noop() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.active_tab = 0; // Claude タブ

        scroll_down(&mut app);
        // Claude タブではスクロールしない（ターミナルが処理）
    }

    #[test]
    fn test_scroll_down_file() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "line1\nline2\nline3".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![vec![], vec![], vec![]], // 3 行
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 0;

        scroll_down(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 1);
        scroll_down(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 2);
        scroll_down(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 2); // 末尾で止まる
    }

    #[test]
    fn test_scroll_down_file_with_claude() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.claude_tabs = 1;
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "line1\nline2".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![vec![], vec![]], // 2 行
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 1; // ファイルタブ

        scroll_down(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 1);
    }

    #[test]
    fn test_scroll_up_file() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "line1\nline2\nline3".to_string(),
            scroll_offset: 0,
            cursor_line: 2,
            highlighted: vec![vec![], vec![], vec![]], // 3 行
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 0;

        scroll_up(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 1);
    }

    #[test]
    fn test_scroll_up_at_zero() {
        let mut app = app_with_worktree();
        let wt = app.selected_worktree_mut().unwrap();
        wt.open_files.push(OpenFile {
            path: PathBuf::from("/tmp/test.rs"),
            content: "hello".to_string(),
            scroll_offset: 0,
            cursor_line: 0,
            highlighted: vec![],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
        });
        wt.active_tab = 0;

        scroll_up(&mut app);
        assert_eq!(app.selected_worktree().unwrap().open_files[0].cursor_line, 0); // 0 で止まる
    }

    #[test]
    fn test_next_tab_no_selected_worktree() {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
            },
            projects: vec![],
        };
        let mut app = App::new(&config);
        next_tab(&mut app);
    }

    #[test]
    fn test_scroll_no_selected_worktree() {
        let config = Config {
            siki: SikiConfig {
                shell: None,
                shared_dirs: vec![],
            },
            projects: vec![],
        };
        let mut app = App::new(&config);
        scroll_down(&mut app);
        scroll_up(&mut app);
    }
}

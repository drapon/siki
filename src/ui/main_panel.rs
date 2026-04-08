use crate::app::{App, OpenFile};
use super::grep_view;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

/// 中央パネルの描画
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    focused: bool,
    claude_screen: Option<&vt100::Screen>,
    grep_rows: &[grep_view::DisplayRow],
) {
    // worktree の情報を先に取得（借用の衝突回避）
    let wt_info = app.selected_worktree().map(|wt| {
        (wt.active_tab, wt.claude_tabs, wt.open_files.len())
    });

    let Some((active_tab, claude_tabs, _open_file_count)) = wt_info else {
        app.claude_content_area = None;
        // worktree 未選択時
        let block = panel_block("Main", focused);
        frame.render_widget(
            Paragraph::new("← プロジェクトから worktree を選択してください")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    };

    // ブランチ情報ヘッダー + タブバー + コンテンツの分割
    let chunks = Layout::vertical([
        Constraint::Length(1), // ブランチ / PR タイトル
        Constraint::Length(2), // タブバー（1行 + 下線）
        Constraint::Min(0),   // コンテンツ
    ])
    .split(area);

    // ブランチ / PR タイトル描画
    if let Some(wt) = app.selected_worktree() {
        render_branch_header(frame, chunks[0], &wt.branch, wt.pr_title.as_deref(), focused);
    }

    // タブバー描画（Search タブを含む）
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree() {
        render_tab_bar(frame, chunks[1], wt.active_tab, wt.claude_tabs, &wt.open_files, has_search, focused);
    }

    // コンテンツ描画
    // タブ構成: [Claude 0..N] [Search?] [File 0..N]
    let search_tab_index = if has_search { Some(claude_tabs) } else { None };

    if Some(active_tab) == search_tab_index {
        // Search タブ
        app.claude_content_area = None;
        app.file_content_area = None;
        grep_view::render(frame, chunks[2], app, grep_rows, focused);
    } else if active_tab < claude_tabs {
        // Claude タブ
        app.file_content_area = None;
        if let Some(screen) = claude_screen {
            render_claude_terminal(frame, chunks[2], screen, focused, app);
        } else {
            app.claude_content_area = None;
            let block = panel_block("Claude Code", focused);
            frame.render_widget(
                Paragraph::new("起動中...")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                chunks[2],
            );
        }
    } else {
        app.claude_content_area = None;
        let file_offset = if has_search { claude_tabs + 1 } else { claude_tabs };
        let file_index = active_tab - file_offset;
        let file_sel = app.text_selection.as_ref().and_then(|s| {
            if s.panel == crate::selection::SelectionPanel::File {
                Some(s.clone())
            } else {
                None
            }
        });
        if let Some(wt) = app.selected_worktree_mut() {
            if let Some(file) = wt.open_files.get_mut(file_index) {
                let content_rect = render_file(frame, chunks[2], file, focused, file_sel.as_ref());
                app.file_content_area = Some(content_rect);
            } else {
                app.file_content_area = None;
            }
        } else {
            app.file_content_area = None;
        }
    }
}

/// ブランチ名 / PR タイトルのヘッダーを描画
fn render_branch_header(
    frame: &mut Frame,
    area: Rect,
    branch: &str,
    pr_title: Option<&str>,
    focused: bool,
) {
    let branch_style = Style::default()
        .fg(if focused { Color::Green } else { Color::DarkGray })
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(branch, branch_style),
    ];

    if let Some(title) = pr_title {
        spans.push(Span::styled(
            " | ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            title,
            Style::default().fg(if focused { Color::Yellow } else { Color::DarkGray }),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// タブバーを描画
fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    active_tab: usize,
    claude_tabs: usize,
    open_files: &[OpenFile],
    has_search: bool,
    focused: bool,
) {
    let titles = build_tab_titles(claude_tabs, open_files, has_search);

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
    app: &mut App,
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

    // コンテンツ領域を計算して保存（ボーダー内側）
    let content_area = block.inner(area);
    app.claude_content_area = Some(content_area);

    let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);
    frame.render_widget(pseudo_term, area);

    // 選択範囲のハイライト描画（Claude パネルの選択のみ）
    if let Some(ref sel) = app.text_selection {
        if sel.panel != crate::selection::SelectionPanel::Claude {
            return;
        }
        let (start, end) = sel.normalize();
        let buf = frame.buffer_mut();
        for row in start.row..=end.row {
            let screen_y = content_area.y + row;
            if screen_y >= content_area.y + content_area.height {
                break;
            }
            let col_start = if row == start.row { start.col } else { 0 };
            let col_end = if row == end.row {
                end.col
            } else {
                content_area.width.saturating_sub(1)
            };
            for col in col_start..=col_end {
                let screen_x = content_area.x + col;
                if screen_x >= content_area.x + content_area.width {
                    break;
                }
                let cell = &mut buf[(screen_x, screen_y)];
                // fg/bg 反転で選択をハイライト
                let fg = cell.fg;
                let bg = cell.bg;
                cell.set_fg(if bg == Color::Reset { Color::Black } else { bg });
                cell.set_bg(if fg == Color::Reset { Color::White } else { fg });
            }
        }
    }
}

/// ファイル内容をシンタックスハイライト付きで描画（キャッシュ済みデータを使用）
/// ファイルを描画し、コンテンツ領域の Rect を返す
fn render_file(
    frame: &mut Frame,
    area: Rect,
    file: &mut OpenFile,
    focused: bool,
    selection: Option<&crate::selection::TextSelection>,
) -> Rect {
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

    // 選択範囲を正規化 (ファイル行に変換)
    let sel_range = selection.and_then(|sel| {
        if sel.is_empty() {
            return None;
        }
        let (s, e) = sel.normalize();
        Some((
            file.scroll_offset + s.row as usize,
            s.col,
            file.scroll_offset + e.row as usize,
            e.col,
        ))
    });

    const LINE_NUM_WIDTH: u16 = 5;

    let lines: Vec<Line> = file
        .highlighted
        .iter()
        .enumerate()
        .map(|(i, spans)| {
            let is_cursor_line = i == file.cursor_line;
            let is_current_match = current_match_line == Some(i);
            let is_other_match = !is_current_match && match_set.contains(&i);

            // この行が選択範囲内かどうか
            let in_selection = sel_range
                .map(|(sr, _, er, _)| i >= sr && i <= er)
                .unwrap_or(false);

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

            if in_selection {
                let (sr, sc, er, ec) = sel_range.unwrap();
                // この行での選択開始/終了カラム (テキスト内の文字位置)
                let sel_start = if i == sr { sc.saturating_sub(LINE_NUM_WIDTH) as usize } else { 0 };
                let sel_end = if i == er { ec.saturating_sub(LINE_NUM_WIDTH) as usize + 1 } else { usize::MAX };

                let mut col: usize = 0;
                for (r, g, b, text) in spans {
                    let span_end = col + text.len();
                    if span_end <= sel_start || col >= sel_end {
                        // 選択範囲外
                        let mut style = Style::default().fg(Color::Rgb(*r, *g, *b));
                        if is_cursor_line {
                            style = style.bg(Color::Rgb(40, 40, 50));
                        }
                        line_spans.push(Span::styled(text.clone(), style));
                    } else {
                        // 部分的 or 全体が選択範囲内
                        let start_in_span = sel_start.saturating_sub(col);
                        let end_in_span = sel_end.saturating_sub(col).min(text.len());

                        // 選択前の部分
                        if start_in_span > 0 {
                            let mut style = Style::default().fg(Color::Rgb(*r, *g, *b));
                            if is_cursor_line { style = style.bg(Color::Rgb(40, 40, 50)); }
                            line_spans.push(Span::styled(text[..start_in_span].to_string(), style));
                        }
                        // 選択部分
                        let sel_style = Style::default()
                            .fg(Color::Rgb(*r, *g, *b))
                            .bg(Color::Rgb(60, 60, 100));
                        line_spans.push(Span::styled(text[start_in_span..end_in_span].to_string(), sel_style));
                        // 選択後の部分
                        if end_in_span < text.len() {
                            let mut style = Style::default().fg(Color::Rgb(*r, *g, *b));
                            if is_cursor_line { style = style.bg(Color::Rgb(40, 40, 50)); }
                            line_spans.push(Span::styled(text[end_in_span..].to_string(), style));
                        }
                    }
                    col = span_end;
                }
            } else {
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
            }
            Line::from(line_spans)
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .scroll((file.scroll_offset as u16, 0));

    frame.render_widget(paragraph, content_area);
    content_area
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

/// タブバーのクリック位置からタブインデックスを返す
pub fn tab_index_at(app: &App, area_x: u16, click_x: u16) -> Option<usize> {
    let wt = app.selected_worktree()?;
    let titles = build_tab_titles(wt.claude_tabs, &wt.open_files, app.show_grep_results_view);
    let rel_x = click_x.saturating_sub(area_x) as usize;
    let mut pos: usize = 0;
    for (i, title) in titles.iter().enumerate() {
        let end = pos + title.len();
        if rel_x >= pos && rel_x < end {
            return Some(i);
        }
        // divider "|" = 1文字
        pos = end + 1;
    }
    None
}

/// タブタイトル一覧を構築する (render_tab_bar と共通ロジック)
fn build_tab_titles(claude_tabs: usize, open_files: &[OpenFile], has_search: bool) -> Vec<String> {
    let mut titles: Vec<String> = (0..claude_tabs)
        .map(|i| {
            if claude_tabs == 1 {
                "Claude".to_string()
            } else {
                format!("Claude {}", i + 1)
            }
        })
        .collect();
    if has_search {
        titles.push("Search".to_string());
    }
    for f in open_files {
        let name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".to_string());
        titles.push(name);
    }
    titles
}

/// タブ切替のロジック
pub fn next_tab(app: &mut App) {
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree_mut() {
        let search_count = if has_search { 1 } else { 0 };
        let tab_count = wt.claude_tabs + search_count + wt.open_files.len();
        if tab_count > 0 {
            wt.active_tab = (wt.active_tab + 1) % tab_count;
        }
    }
}

/// ファイルタブを閉じる（Claude タブは Ctrl+\ で閉じる）
pub fn close_current_tab(app: &mut App) {
    let has_search = app.show_grep_results_view;
    // Search タブを閉じる
    if has_search {
        if let Some(wt) = app.selected_worktree() {
            if wt.active_tab == wt.claude_tabs {
                app.show_grep_results_view = false;
                // タブ位置を調整
                if let Some(wt) = app.selected_worktree_mut() {
                    let tab_count = wt.claude_tabs + wt.open_files.len();
                    if wt.active_tab >= tab_count {
                        wt.active_tab = tab_count.saturating_sub(1);
                    }
                }
                return;
            }
        }
    }

    if let Some(wt) = app.selected_worktree_mut() {
        if wt.active_tab < wt.claude_tabs {
            return; // Claude タブはここでは閉じない
        }
        let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
        if wt.active_tab < offset {
            return;
        }
        let file_index = wt.active_tab - offset;
        if file_index < wt.open_files.len() {
            wt.open_files.remove(file_index);
            let search_count = if has_search { 1 } else { 0 };
            let tab_count = wt.claude_tabs + search_count + wt.open_files.len();
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
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree_mut() {
        let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
        // 既に開いているか確認
        for (i, f) in wt.open_files.iter().enumerate() {
            if f.path == path {
                wt.active_tab = offset + i;
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
        wt.active_tab = offset + wt.open_files.len() - 1;
    }
}

/// カーソルを下に移動（Claude タブはターミナルが処理するので対象外）
pub fn scroll_down(app: &mut App) {
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree_mut() {
        let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
        if wt.active_tab < offset {
            return;
        }
        let file_index = wt.active_tab - offset;
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
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree_mut() {
        let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
        if wt.active_tab < offset {
            return;
        }
        let file_index = wt.active_tab - offset;
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
    let idx = match file_tab_index(app, wt.active_tab, wt.claude_tabs) {
        Some(i) => i,
        None => return false,
    };
    wt.open_files
        .get(idx)
        .map(|f| f.search_active)
        .unwrap_or(false)
}

/// Search タブ分のオフセットを計算してファイルインデックスを返す
fn file_tab_index(app: &App, active_tab: usize, claude_tabs: usize) -> Option<usize> {
    let offset = if app.show_grep_results_view {
        claude_tabs + 1
    } else {
        claude_tabs
    };
    if active_tab >= offset {
        Some(active_tab - offset)
    } else {
        None
    }
}

/// 現在のファイルに対して検索操作を行うヘルパー
fn with_current_file_mut(app: &mut App, f: impl FnOnce(&mut OpenFile)) {
    let has_search = app.show_grep_results_view;
    if let Some(wt) = app.selected_worktree_mut() {
        let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
        if wt.active_tab >= offset {
            let file_index = wt.active_tab - offset;
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

/// ファイルビューのコンテンツ領域でクリックした行にカーソルを移動する
pub fn click_file_line(app: &mut App, main_area: Rect, click_row: u16) {
    // main 領域の上部: ヘッダー(1) + タブバー(2) + ボーダー(1) = 4行
    let base_top = main_area.y + 4;
    // 検索バーの有無で追加オフセット
    let has_search_bar = {
        let wt = match app.selected_worktree() {
            Some(wt) => wt,
            None => return,
        };
        let file_index = match file_tab_index(app, wt.active_tab, wt.claude_tabs) {
            Some(i) => i,
            None => return,
        };
        match wt.open_files.get(file_index) {
            Some(f) => f.search_active || !f.search_matches.is_empty(),
            None => return,
        }
    };
    let content_top = if has_search_bar { base_top + 1 } else { base_top };
    if click_row < content_top {
        return;
    }
    let clicked_row = (click_row - content_top) as usize;
    with_current_file_mut(app, |file| {
        let target = file.scroll_offset + clicked_row;
        if target < file.highlighted.len() {
            file.cursor_line = target;
        }
    });
}


/// 現在開いているファイルのパス:行番号を返す（1-indexed）
pub fn current_file_location(app: &App) -> Option<String> {
    let wt = app.selected_worktree()?;
    let file_index = file_tab_index(app, wt.active_tab, wt.claude_tabs)?;
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
                base_branch: None,
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
                base_branch: None,
            },
            projects: vec![],
        };
        let mut app = App::new(&config);
        scroll_down(&mut app);
        scroll_up(&mut app);
    }
}

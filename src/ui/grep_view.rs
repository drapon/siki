use crate::app::{App, GrepResult};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::path::Path;

/// grep 結果をファイルごとにグルーピングした表示行
#[derive(Debug, Clone)]
pub enum DisplayRow {
    /// ファイルヘッダー行
    FileHeader {
        rel_path: String,
        match_count: usize,
    },
    /// コンテキスト行（マッチ前後）
    Context { line_number: usize, content: String },
    /// マッチ行
    Match { line_number: usize, content: String },
    /// 省略記号（...）
    Separator,
}

const CONTEXT_LINES: usize = 2;

/// grep 結果から表示行を構築する
pub fn build_display_rows(results: &[GrepResult], wt_path: &Path) -> Vec<DisplayRow> {
    let mut rows = Vec::new();

    // ファイルごとにグルーピング（出現順を保持）
    let mut file_groups: Vec<(String, Vec<&GrepResult>)> = Vec::new();
    let mut file_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for r in results {
        let rel = r
            .path
            .strip_prefix(wt_path)
            .unwrap_or(&r.path)
            .display()
            .to_string();
        if let Some(&idx) = file_index.get(&rel) {
            file_groups[idx].1.push(r);
        } else {
            file_index.insert(rel.clone(), file_groups.len());
            file_groups.push((rel, vec![r]));
        }
    }

    for (rel_path, matches) in &file_groups {
        rows.push(DisplayRow::FileHeader {
            rel_path: rel_path.clone(),
            match_count: matches.len(),
        });

        // ファイルを読み込んでコンテキスト行を取得
        let file_path = if let Some(first) = matches.first() {
            first.path.clone()
        } else {
            continue;
        };
        let file_lines = read_file_lines(&file_path);

        let mut prev_end: usize = 0;

        for m in matches {
            let start = m.line_number.saturating_sub(CONTEXT_LINES).max(1);
            let end = m.line_number + CONTEXT_LINES;

            if prev_end > 0 && start > prev_end + 1 {
                rows.push(DisplayRow::Separator);
            }

            let actual_start = if prev_end > 0 && start <= prev_end {
                prev_end + 1
            } else {
                start
            };

            for ln in actual_start..=end {
                let content = file_lines
                    .get(ln.saturating_sub(1))
                    .cloned()
                    .unwrap_or_default();

                if ln == m.line_number {
                    rows.push(DisplayRow::Match {
                        line_number: ln,
                        content,
                    });
                } else {
                    rows.push(DisplayRow::Context {
                        line_number: ln,
                        content,
                    });
                }
            }

            prev_end = end;
        }
    }

    rows
}

/// 現在のカーソル位置の "path:line" 形式文字列を取得
pub fn location_string_at(rows: &[DisplayRow], cursor: usize) -> Option<String> {
    let mut current_file = String::new();
    for (i, row) in rows.iter().enumerate() {
        match row {
            DisplayRow::FileHeader { rel_path, .. } => {
                current_file = rel_path.clone();
            }
            DisplayRow::Match { line_number, .. } | DisplayRow::Context { line_number, .. } => {
                if i == cursor {
                    return Some(format!("{}:{}", current_file, line_number));
                }
            }
            DisplayRow::Separator => {}
        }
        if i == cursor {
            if let DisplayRow::FileHeader { rel_path, .. } = row {
                return Some(rel_path.clone());
            }
        }
    }
    None
}

/// 現在のカーソル位置のファイルパスと行番号を取得
pub fn location_at(
    rows: &[DisplayRow],
    cursor: usize,
    results: &[GrepResult],
) -> Option<(std::path::PathBuf, usize)> {
    let mut current_file_rel = String::new();
    for (i, row) in rows.iter().enumerate() {
        match row {
            DisplayRow::FileHeader { rel_path, .. } => {
                current_file_rel = rel_path.clone();
            }
            DisplayRow::Match { line_number, .. } | DisplayRow::Context { line_number, .. } => {
                if i == cursor {
                    // rel_path に一致する結果からフルパスを取得
                    let full_path = results.iter().find(|r| {
                        r.path.display().to_string().ends_with(&current_file_rel)
                    });
                    if let Some(r) = full_path {
                        return Some((r.path.clone(), *line_number));
                    }
                }
            }
            _ => {}
        }
        if i == cursor {
            if let DisplayRow::FileHeader { .. } = row {
                let full_path = results.iter().find(|r| {
                    r.path.display().to_string().ends_with(&current_file_rel)
                });
                if let Some(r) = full_path {
                    return Some((r.path.clone(), r.line_number));
                }
            }
        }
    }
    None
}

/// 中央ペインに grep 結果を描画
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    rows: &[DisplayRow],
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Search: \"{}\"", app.grep_input))
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("  (マッチなし)").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let visible_height = inner.height as usize;
    let scroll = app.grep_view_scroll;

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, row)| render_row(i, row, &app.grep_input, i == app.grep_view_cursor, focused))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_row(
    _index: usize,
    row: &DisplayRow,
    query: &str,
    is_cursor: bool,
    focused: bool,
) -> Line<'static> {
    let cursor_bg = if is_cursor && focused {
        Color::Rgb(50, 50, 80)
    } else if is_cursor {
        Color::Rgb(40, 40, 50)
    } else {
        Color::Reset
    };

    match row {
        DisplayRow::FileHeader {
            rel_path,
            match_count,
        } => {
            let header_bg = if is_cursor && focused {
                cursor_bg
            } else {
                Color::Rgb(30, 30, 40)
            };
            Line::from(vec![
                Span::styled(
                    format!("  {} ", rel_path),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                        .bg(header_bg),
                ),
                Span::styled(
                    format!(" {} matches ", match_count),
                    Style::default().fg(Color::Rgb(140, 140, 140)).bg(header_bg),
                ),
            ])
        }
        DisplayRow::Match {
            line_number,
            content,
        } => {
            let ln = format!(" {:>4} │", line_number);
            let mut spans = vec![Span::styled(
                ln,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
                    .bg(cursor_bg),
            )];
            spans.extend(highlight_matches(content, query, cursor_bg));
            Line::from(spans)
        }
        DisplayRow::Context {
            line_number,
            content,
        } => {
            let ln = format!(" {:>4} │", line_number);
            Line::from(vec![
                Span::styled(ln, Style::default().fg(Color::Rgb(80, 80, 80)).bg(cursor_bg)),
                Span::styled(
                    content.to_string(),
                    Style::default().fg(Color::Rgb(120, 120, 120)).bg(cursor_bg),
                ),
            ])
        }
        DisplayRow::Separator => Line::from(Span::styled(
            "      ···".to_string(),
            Style::default().fg(Color::Rgb(80, 80, 80)).bg(cursor_bg),
        )),
    }
}

/// クエリにマッチする部分をハイライト
fn highlight_matches(text: &str, query: &str, bg: Color) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(
            text.to_string(),
            Style::default().bg(bg),
        )];
    }

    let mut spans = Vec::new();
    let lower_text = text.to_lowercase();
    let lower_query = query.to_lowercase();
    let mut last = 0;

    for (start, _) in lower_text.match_indices(&lower_query) {
        if start > last {
            spans.push(Span::styled(
                text[last..start].to_string(),
                Style::default().fg(Color::White).bg(bg),
            ));
        }
        spans.push(Span::styled(
            text[start..start + query.len()].to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        last = start + query.len();
    }
    if last < text.len() {
        spans.push(Span::styled(
            text[last..].to_string(),
            Style::default().fg(Color::White).bg(bg),
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            text.to_string(),
            Style::default().bg(bg),
        ));
    }
    spans
}

fn read_file_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_display_rows_empty() {
        let rows = build_display_rows(&[], Path::new("/tmp"));
        assert!(rows.is_empty());
    }

    #[test]
    fn test_highlight_matches_basic() {
        let spans = highlight_matches("hello world hello", "hello", Color::Reset);
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn test_highlight_no_match() {
        let spans = highlight_matches("no match here", "xyz", Color::Reset);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn test_highlight_empty_query() {
        let spans = highlight_matches("some text", "", Color::Reset);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn test_location_string_at_empty() {
        assert!(location_string_at(&[], 0).is_none());
    }

    #[test]
    fn test_location_string_at_file_header() {
        let rows = vec![DisplayRow::FileHeader {
            rel_path: "src/main.rs".to_string(),
            match_count: 1,
        }];
        assert_eq!(
            location_string_at(&rows, 0),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn test_location_string_at_match_line() {
        let rows = vec![
            DisplayRow::FileHeader {
                rel_path: "src/main.rs".to_string(),
                match_count: 1,
            },
            DisplayRow::Match {
                line_number: 42,
                content: "fn main()".to_string(),
            },
        ];
        assert_eq!(
            location_string_at(&rows, 1),
            Some("src/main.rs:42".to_string())
        );
    }
}

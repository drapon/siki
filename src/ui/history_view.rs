use chrono::{DateTime, Local, TimeZone, Utc};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use rusqlite::Connection;

use crate::db;

/// 履歴エントリ（conversation_logs から取得）
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub session_id: String,
    pub summary: Option<String>,
    pub created_at: i64,
}

/// セッション履歴ビュー
#[derive(Debug)]
pub struct HistoryView {
    pub entries: Vec<HistoryEntry>,
    pub selected: usize,
    pub summary_scroll: usize,
}

impl HistoryView {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            summary_scroll: 0,
        }
    }

    /// DB から指定 worktree の会話ログを読み込む
    pub fn load(&mut self, conn: &Connection, worktree_name: &str, project_name: &str) {
        self.selected = 0;
        self.summary_scroll = 0;
        match db::get_conversation_logs_by_worktree(conn, worktree_name, project_name) {
            Ok(logs) => {
                // 新しい順に表示
                self.entries = logs
                    .into_iter()
                    .rev()
                    .map(|log| HistoryEntry {
                        session_id: log.session_id,
                        summary: log.summary,
                        created_at: log.created_at,
                    })
                    .collect();
            }
            Err(_) => {
                self.entries.clear();
            }
        }
    }

    pub fn select_next(&mut self) {
        if !self.entries.is_empty() && self.selected < self.entries.len() - 1 {
            self.selected += 1;
            self.summary_scroll = 0;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.summary_scroll = 0;
        }
    }

    pub fn scroll_down(&mut self) {
        self.summary_scroll += 1;
    }

    pub fn scroll_up(&mut self) {
        self.summary_scroll = self.summary_scroll.saturating_sub(1);
    }

    /// 選択中のエントリのセッション ID を返す
    pub fn selected_session_id(&self) -> Option<&str> {
        self.entries.get(self.selected).map(|e| e.session_id.as_str())
    }

    /// 描画（上: セッション一覧、下: サマリ詳細）
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        if self.entries.is_empty() {
            let block = Block::default()
                .borders(Borders::ALL)
                .title("History")
                .border_style(border_style);
            frame.render_widget(
                Paragraph::new("(no history)")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        // 上下分割: セッション一覧 / サマリ詳細
        let chunks = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .split(area);

        self.render_session_list(frame, chunks[0], focused, border_style);
        self.render_summary_detail(frame, chunks[1], focused, border_style);
    }

    fn render_session_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        border_style: Style,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("History (y: copy ID  Enter: resume)")
            .border_style(border_style);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines: Vec<Line> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_selected = i == self.selected;
                let base_style = if is_selected && focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                let time_str = format_timestamp(entry.created_at);
                let summary_str = entry
                    .summary
                    .as_deref()
                    .unwrap_or("(no summary)")
                    .lines()
                    .next()
                    .unwrap_or("(no summary)");

                // 表示幅に収まるようにトリミング
                let available = inner.width as usize;
                let time_width = time_str.len() + 1; // +1 for space
                let max_summary = available.saturating_sub(time_width + 1);
                let truncated: String = if summary_str.chars().count() > max_summary {
                    let s: String = summary_str.chars().take(max_summary.saturating_sub(1)).collect();
                    format!("{}…", s)
                } else {
                    summary_str.to_string()
                };

                Line::from(vec![
                    Span::styled(
                        format!("{} ", time_str),
                        Style::default().fg(Color::DarkGray).patch(base_style),
                    ),
                    Span::styled(
                        truncated,
                        Style::default().fg(Color::White).patch(base_style),
                    ),
                ])
            })
            .collect();

        let visible_height = inner.height as usize;
        let scroll = if self.selected >= visible_height {
            self.selected - visible_height + 1
        } else {
            0
        };

        let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0));
        frame.render_widget(paragraph, inner);
    }

    fn render_summary_detail(
        &self,
        frame: &mut Frame,
        area: Rect,
        _focused: bool,
        border_style: Style,
    ) {
        let entry = match self.entries.get(self.selected) {
            Some(e) => e,
            None => return,
        };

        let short_id = if entry.session_id.len() > 12 {
            &entry.session_id[..12]
        } else {
            &entry.session_id
        };
        let title = format!("Summary [{}…]", short_id);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style);

        let content = entry
            .summary
            .as_deref()
            .unwrap_or("(no summary available)");

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((self.summary_scroll as u16, 0))
            .style(Style::default().fg(Color::Gray));

        frame.render_widget(paragraph, area);
    }
}

fn format_timestamp(unix_secs: i64) -> String {
    match Utc.timestamp_opt(unix_secs, 0) {
        chrono::LocalResult::Single(dt) => {
            let local: DateTime<Local> = dt.into();
            local.format("%m/%d %H:%M").to_string()
        }
        _ => "??/??".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_view_new() {
        let view = HistoryView::new();
        assert!(view.entries.is_empty());
        assert_eq!(view.selected, 0);
        assert_eq!(view.summary_scroll, 0);
    }

    #[test]
    fn test_select_navigation() {
        let mut view = HistoryView::new();
        view.entries = vec![
            HistoryEntry {
                session_id: "s1".into(),
                summary: Some("First".into()),
                created_at: 1000,
            },
            HistoryEntry {
                session_id: "s2".into(),
                summary: Some("Second".into()),
                created_at: 2000,
            },
        ];

        assert_eq!(view.selected, 0);
        view.select_next();
        assert_eq!(view.selected, 1);
        view.select_next(); // at end
        assert_eq!(view.selected, 1);
        view.select_prev();
        assert_eq!(view.selected, 0);
        view.select_prev(); // at start
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn test_select_resets_scroll() {
        let mut view = HistoryView::new();
        view.entries = vec![
            HistoryEntry {
                session_id: "s1".into(),
                summary: Some("First".into()),
                created_at: 1000,
            },
            HistoryEntry {
                session_id: "s2".into(),
                summary: Some("Second".into()),
                created_at: 2000,
            },
        ];

        view.summary_scroll = 5;
        view.select_next();
        assert_eq!(view.summary_scroll, 0);
    }

    #[test]
    fn test_selected_session_id() {
        let mut view = HistoryView::new();
        assert!(view.selected_session_id().is_none());

        view.entries = vec![HistoryEntry {
            session_id: "test-id-123".into(),
            summary: None,
            created_at: 1000,
        }];
        assert_eq!(view.selected_session_id(), Some("test-id-123"));
    }

    #[test]
    fn test_scroll() {
        let mut view = HistoryView::new();
        assert_eq!(view.summary_scroll, 0);
        view.scroll_down();
        assert_eq!(view.summary_scroll, 1);
        view.scroll_down();
        assert_eq!(view.summary_scroll, 2);
        view.scroll_up();
        assert_eq!(view.summary_scroll, 1);
        view.scroll_up();
        assert_eq!(view.summary_scroll, 0);
        view.scroll_up(); // doesn't go below 0
        assert_eq!(view.summary_scroll, 0);
    }

    #[test]
    fn test_format_timestamp() {
        // Just verify it doesn't panic
        let result = format_timestamp(1700000000);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_load_from_db() {
        let conn = db::init(std::path::Path::new(":memory:")).unwrap();
        db::upsert_conversation_log(&conn, "s1", "wt1", "proj1", Some("main"), "[]").unwrap();
        db::update_conversation_log_summary(&conn, "s1", "Test summary").unwrap();

        let mut view = HistoryView::new();
        view.load(&conn, "wt1", "proj1");
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].session_id, "s1");
        assert_eq!(view.entries[0].summary.as_deref(), Some("Test summary"));
    }
}

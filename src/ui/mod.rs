pub mod diff_view;
pub mod layout;
pub mod left_panel;
pub mod main_panel;
pub mod right_panel;
pub mod source_tree;
pub mod syntax;

use crate::app::{App, Panel};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use diff_view::DiffView;
use source_tree::SourceTree;

/// ターミナルタブ情報（描画用）
pub struct TerminalTabInfo {
    /// 存在するタブのインデックス一覧
    pub tabs: Vec<usize>,
    /// アクティブなタブインデックス
    pub active: usize,
}

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    left_panel: &left_panel::LeftPanel,
    source_tree: &SourceTree,
    diff_view: &DiffView,
    terminal_screen: Option<&vt100::Screen>,
    terminal_tab_info: Option<&TerminalTabInfo>,
    claude_screen: Option<&vt100::Screen>,
) -> layout::AppLayout {
    let areas = layout::compute_layout(frame.area());

    // 左パネル
    left_panel.render(frame, areas.left, app, app.focused_panel == Panel::Left);

    // 中央パネル
    main_panel::render(
        frame,
        areas.main,
        app,
        app.focused_panel == Panel::Main,
        claude_screen,
    );

    // 右パネル上部（SourceTree / DiffView）
    right_panel::render_top(
        frame,
        areas.right_top,
        app,
        source_tree,
        diff_view,
        app.focused_panel == Panel::Right,
    );

    // 右パネル下部（ターミナル）
    render_terminal(
        frame,
        areas.right_bottom,
        app,
        terminal_screen,
        terminal_tab_info,
    );

    // ステータスバー
    if let Some(ref msg) = app.status_message {
        let style = match msg.level {
            crate::app::StatusLevel::Info => Style::default().fg(Color::Green),
            crate::app::StatusLevel::Error => Style::default().fg(Color::Red),
        };
        frame.render_widget(
            Paragraph::new(msg.text.as_str()).style(style),
            areas.status_bar,
        );
    }

    // ヘルプポップアップ
    if app.show_help {
        render_help_popup(frame, app);
    }

    // メッセージ入力ポップアップ
    if app.show_message_popup {
        render_message_popup(frame, app);
    }

    // Worktree 追加ポップアップ
    if app.show_add_worktree_popup {
        render_add_worktree_popup(frame, app);
    }

    // プロジェクト追加ポップアップ
    if app.show_add_project_popup {
        render_add_project_popup(frame, app);
    }

    // Grep 検索ポップアップ
    if app.show_grep_popup {
        render_grep_popup(frame, app);
    }

    // アーカイブ確認ダイアログ
    if app.show_archive_confirm {
        render_archive_confirm_popup(frame, app);
    }

    areas
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block.border_style(Style::default().fg(Color::Cyan))
    } else {
        block.border_style(Style::default().fg(Color::DarkGray))
    }
}

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 70, frame.area());
    let help_text = vec![
        "キーバインド一覧",
        "",
        "[グローバル]",
        "  q          : 終了",
        "  ?/F1       : ヘルプ表示",
        "  Tab        : 次のパネルへ",
        "  Shift+Tab  : 前のパネルへ",
        "  クリック    : パネル切替",
        "  ホイール    : スクロール",
        "",
        "[左パネル]",
        "  j/↓        : カーソル下",
        "  k/↑        : カーソル上",
        "  Space      : 折りたたみ/展開",
        "  Enter      : worktree 選択",
        "  a          : worktree 追加",
        "  A          : プロジェクト追加",
        "  r          : run スクリプト実行",
        "  d          : worktree アーカイブ",
        "",
        "[中央パネル]",
        "  Tab        : 次のタブ",
        "  w          : タブを閉じる",
        "  i          : Claude Code 起動",
        "  Ctrl+\\     : Claude タブから離脱",
        "  j/k/↑/↓    : カーソル行移動",
        "  /          : ファイル内検索",
        "  n/N        : 次/前の検索結果",
        "  g          : 全体検索 (grep)",
        "  s          : ファイル:行を Claude に送信",
        "",
        "[右パネル上部]",
        "  t          : Tree/Diff 切替",
        "  j/k/↑/↓    : カーソル/スクロール",
        "  h/l/←/→    : 折りたたみ/展開",
        "  Enter      : ファイルを開く",
        "  /          : ファイル検索",
        "  n/N        : 次/前の検索結果",
        "",
        "[ターミナル]",
        "  n          : ターミナル起動",
        "  Ctrl+n     : 新しいタブ",
        "  Ctrl+1-5   : タブ切替",
        "  Ctrl+\\     : ターミナルから離脱",
        "",
        "j/k でスクロール  Esc で閉じる",
    ];

    let total_lines = help_text.len() as u16;

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Help")
        .border_style(Style::default().fg(Color::Yellow));
    let inner_height = block.inner(area).height;

    let max_scroll = total_lines.saturating_sub(inner_height) as usize;
    let scroll = app.help_scroll.min(max_scroll);

    let paragraph = Paragraph::new(help_text.join("\n"))
        .block(block)
        .scroll((scroll as u16, 0));

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_message_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 30, frame.area());
    let title = if let Some(wt) = app.selected_worktree() {
        format!("Message to: {}", wt.name)
    } else {
        "Message".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    let input_text = format!("{}_", app.popup_input);
    let paragraph = Paragraph::new(input_text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_add_project_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 30, frame.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .title("プロジェクト追加")
        .border_style(Style::default().fg(Color::Yellow));

    let text = format!(
        "\n  パス: {}_\n\n  Enter: 追加  Esc: キャンセル",
        app.add_project_input,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_add_worktree_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 30, frame.area());
    let project_name = app
        .projects
        .get(app.add_worktree_project_index)
        .map(|p| p.name.as_str())
        .unwrap_or("???");

    let title = format!("Worktree 追加: {}", project_name);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let text = format!(
        "\n  名前: {}\n  ブランチ: {}_\n\n  Enter: 追加  Esc: キャンセル",
        app.add_worktree_name, app.add_worktree_input,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_grep_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 60, frame.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Grep 検索")
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(block.clone(), area);

    let inner = block.inner(area);
    let chunks = Layout::vertical([
        Constraint::Length(1), // 入力行
        Constraint::Length(1), // 区切り
        Constraint::Min(0),    // 結果リスト
    ])
    .split(inner);

    // 入力行
    let input_text = if app.grep_results.is_empty() && !app.grep_input.is_empty() {
        format!("grep: {}_ (Enter で検索)", app.grep_input)
    } else if !app.grep_results.is_empty() {
        format!(
            "grep: {} [{}/{}]",
            app.grep_input,
            if app.grep_results.is_empty() {
                0
            } else {
                app.grep_cursor + 1
            },
            app.grep_results.len()
        )
    } else {
        format!("grep: {}_", app.grep_input)
    };
    frame.render_widget(
        Paragraph::new(input_text).style(Style::default().fg(Color::Yellow)),
        chunks[0],
    );

    // 区切り線
    frame.render_widget(
        Paragraph::new("─".repeat(chunks[1].width as usize))
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );

    // 結果リスト
    if app.grep_results.is_empty() {
        if !app.grep_input.is_empty() {
            frame.render_widget(
                Paragraph::new("  (マッチなし)").style(Style::default().fg(Color::DarkGray)),
                chunks[2],
            );
        }
    } else {
        let visible_height = chunks[2].height as usize;
        // カーソルが見えるようにスクロール
        let scroll = if app.grep_cursor >= visible_height {
            app.grep_cursor - visible_height + 1
        } else {
            0
        };

        let lines: Vec<Line> = app
            .grep_results
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_height)
            .map(|(i, result)| {
                let is_selected = i == app.grep_cursor;
                let path_str = result
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| result.path.display().to_string());
                let text = format!(
                    " {}:{} {}",
                    path_str, result.line_number, result.line_content
                );
                if is_selected {
                    Line::styled(
                        text,
                        Style::default().fg(Color::Cyan).bg(Color::Rgb(30, 30, 50)),
                    )
                } else {
                    Line::styled(text, Style::default().fg(Color::White))
                }
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), chunks[2]);
    }
}

fn render_archive_confirm_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(40, 20, frame.area());

    let (wt_name, wt_branch) = app
        .archive_target
        .and_then(|id| app.worktree_by_id(id))
        .map(|wt| (wt.name.as_str(), wt.branch.as_str()))
        .unwrap_or(("???", "???"));

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Worktree アーカイブ")
        .border_style(Style::default().fg(Color::Red));

    let text = format!(
        "\n  \"{}\" ({})\n  をアーカイブしますか？\n\n  y: アーカイブ  n: キャンセル",
        wt_name, wt_branch,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    terminal_screen: Option<&vt100::Screen>,
    tab_info: Option<&TerminalTabInfo>,
) {
    let focused = app.focused_panel == Panel::Terminal;

    // タブバーのタイトルを構築
    let title = match tab_info {
        Some(info) if !info.tabs.is_empty() => {
            let tabs_str: String = info
                .tabs
                .iter()
                .map(|&i| {
                    if i == info.active {
                        format!("[{}]", i + 1)
                    } else {
                        format!(" {} ", i + 1)
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            if focused {
                format!("Terminal {} (Ctrl+\\ exit)", tabs_str)
            } else {
                format!("Terminal {}", tabs_str)
            }
        }
        _ => {
            if focused {
                "Terminal (n: new)".to_string()
            } else {
                "Terminal".to_string()
            }
        }
    };

    let block = panel_block(&title, focused);

    match terminal_screen {
        Some(screen) => {
            let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);
            frame.render_widget(pseudo_term, area);
        }
        None => {
            let hint = if focused {
                "n キーでターミナルを起動"
            } else {
                "(ターミナル未起動)"
            };
            frame.render_widget(
                Paragraph::new(hint)
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

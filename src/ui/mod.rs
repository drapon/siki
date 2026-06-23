pub mod diff_view;
pub mod grep_view;
pub mod history_view;
pub mod layout;
pub mod left_panel;
pub mod main_panel;
pub mod right_panel;
pub mod source_tree;
pub mod syntax;

use crate::app::{App, Panel};
use crate::selection::SelectionPanel;
use crate::session::{SessionRegistry, SessionState};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use diff_view::{DiffView, LocalChangesView};
use history_view::HistoryView;
use source_tree::SourceTree;

/// ターミナルタブ情報（描画用）
pub struct TerminalTabInfo {
    /// 存在するタブのインデックス一覧
    pub tabs: Vec<usize>,
    /// アクティブなタブインデックス
    pub active: usize,
}

/// ターミナルタイトルのプレフィックス
/// 対応する描画: render_terminal() 内の format!("Terminal {}", tabs_str)
pub const TERMINAL_TITLE_PREFIX: &str = "Terminal ";
/// 各タブの表示幅
/// 対応する描画: render_terminal() 内の format!("[{}]", i + 1) / format!(" {} ", i + 1)
pub const TERMINAL_TAB_WIDTH: usize = 3;
/// ターミナルタブの最大数
pub const MAX_TERMINAL_TABS: usize = 5;

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    left_panel: &mut left_panel::LeftPanel,
    source_tree: &mut SourceTree,
    diff_view: &DiffView,
    local_changes: &LocalChangesView,
    history_view: &HistoryView,
    terminal_screen: Option<&vt100::Screen>,
    terminal_tab_info: Option<&TerminalTabInfo>,
    claude_screen: Option<&vt100::Screen>,
    siki_init_screen: Option<&vt100::Screen>,
    session_registry: Option<&SessionRegistry>,
    grep_rows: &[grep_view::DisplayRow],
) -> layout::AppLayout {
    let areas = layout::compute_layout(frame.area());

    // 左パネル
    left_panel.render(frame, areas.left, app, app.focused_panel == Panel::Left, session_registry);

    // 中央パネル
    main_panel::render(
        frame,
        areas.main,
        app,
        app.focused_panel == Panel::Main,
        claude_screen,
        grep_rows,
    );

    // 右パネル上部（SourceTree / DiffView / History）
    right_panel::render_top(
        frame,
        areas.right_top,
        app,
        source_tree,
        diff_view,
        local_changes,
        history_view,
        app.focused_panel == Panel::Right,
    );

    // 右パネル下部（ターミナル）
    render_terminal(
        frame,
        areas.right_bottom,
        &mut *app,
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

    // プロジェクト表示名変更ポップアップ
    if app.show_rename_project_popup {
        render_rename_project_popup(frame, app);
    }

    // Grep 検索ポップアップ
    if app.show_grep_popup {
        render_grep_popup(frame, app);
    }

    // アーカイブ確認ダイアログ
    if app.show_archive_confirm {
        render_archive_confirm_popup(frame, app);
    }

    // プロジェクト除外確認ダイアログ
    if app.show_remove_project_confirm {
        render_remove_project_confirm_popup(frame, app);
    }

    // siki.json 作成確認ダイアログ
    if app.show_siki_json_confirm {
        render_siki_json_confirm_popup(frame);
    }

    if app.show_llm_picker {
        render_llm_picker_popup(frame, &app.available_llms, app.llm_picker_cursor);
    }

    if app.show_session_choice {
        render_session_choice_popup(frame);
    }

    // siki.json 作成オーバーレイターミナル
    if app.show_siki_json_init_terminal {
        render_siki_json_init_terminal(
            frame,
            siki_init_screen,
            app.siki_json_init_scroll,
            app.siki_json_init_spinner,
        );
    }

    // スキル名入力ポップアップ
    if app.show_skill_name_popup {
        render_skill_name_popup(frame, app);
    }

    // スキル内容入力ポップアップ
    if app.show_skill_edit_popup {
        render_skill_edit_popup(frame, app);
    }

    // スキル一覧ポップアップ
    if app.show_skill_list {
        render_skill_list_popup(frame, app);
    }

    // コンテキスト一覧ポップアップ
    if app.show_context_list {
        render_context_list_popup(frame, app);
    }

    // コンテキスト名入力ポップアップ
    if app.show_context_name_popup {
        render_context_name_popup(frame, app);
    }

    // コンテキスト編集ポップアップ
    if app.show_context_edit_popup {
        render_context_edit_popup(frame, app);
    }

    // コンテキスト URL 入力ポップアップ
    if app.show_context_url_popup {
        render_context_url_popup(frame, app);
    }

    // シンボリックリンク設定ポップアップ
    if app.show_symlink_settings {
        render_symlink_settings_popup(frame, app);
    }

    areas
}

fn render_llm_picker_popup(frame: &mut Frame, llms: &[String], cursor: usize) {
    use ratatui::widgets::{Clear, List, ListItem, ListState};

    let area = frame.area();
    let item_count = llms.len() as u16;
    let w = 30.min(area.width);
    let h = (item_count + 2).min(area.height); // +2 for border
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Select LLM")
        .border_style(Style::default().fg(Color::Cyan));

    let items: Vec<ListItem> = llms
        .iter()
        .map(|name| ListItem::new(format!(" {}", name)))
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default().with_selected(Some(cursor));
    frame.render_stateful_widget(list, popup_area, &mut state);
}

fn render_session_choice_popup(frame: &mut Frame) {
    use ratatui::widgets::{Clear, Paragraph};

    let area = frame.area();
    let w = 44.min(area.width);
    let h = 7.min(area.height);
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Active Session Found")
        .border_style(Style::default().fg(Color::Yellow));
    let text = " [1] Start new (default)\n [2] Resume session\n [3] Continue with context";
    frame.render_widget(
        Paragraph::new(text).block(block).style(Style::default().fg(Color::White)),
        popup_area,
    );
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
        "  ドラッグ    : テキスト選択",
        "  y          : 選択範囲をコピー",
        "  ホイール    : スクロール",
        "",
        "[左パネル]",
        "  j/↓        : カーソル下",
        "  k/↑        : カーソル上",
        "  Space      : 折りたたみ/展開",
        "  Enter      : worktree 選択",
        "  a          : worktree 追加",
        "  A          : プロジェクト追加",
        "  R          : プロジェクト表示名変更",
        "  S          : siki.json 作成",
        "  r          : run スクリプト実行",
        "  K          : スキル管理",
        "  C          : コンテキスト管理",
        "  L          : シンボリックリンク設定",
        "  d          : worktree アーカイブ / プロジェクト除外",
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
        "[Claude Code タブ]",
        "  Ctrl+t     : 新しいタブ追加",
        "  Ctrl+w     : タブを閉じる",
        "  Ctrl+r     : セッション再開 (-r)",
        "  Ctrl+g     : grep 検索",
        "  Tab        : 次のタブへ",
        "  Shift+↑/↓  : スクロール (1行)",
        "  Shift+PgUp/PgDn : スクロール (10行)",
        "  Ctrl+\\     : Claude タブから離脱",
        "",
        "[ターミナル]",
        "  Ctrl+t     : 起動 / 新しいタブ",
        "  Ctrl+1-5   : タブ切替",
        "  Ctrl+]     : 次のタブへ循環",
        "  Ctrl+g     : grep 検索",
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

fn render_rename_project_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 30, frame.area());

    let project = match app
        .rename_project_name
        .as_deref()
        .and_then(|name| app.projects.iter().find(|p| p.name == name))
    {
        Some(p) => p,
        None => return, // プロジェクトが削除された場合は描画スキップ
    };
    let title = if let Some((pi, wi)) = app.rename_worktree_target {
        let wt_name = app.projects.get(pi)
            .and_then(|p| p.worktrees.get(wi))
            .map(|wt| wt.name.as_str())
            .unwrap_or("?");
        format!("worktree 表示名変更: {}", wt_name)
    } else {
        format!("表示名変更: {}", project.name)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let len = app.rename_project_input.chars().count();
    let remaining = 100_usize.saturating_sub(len);
    let counter = if remaining <= 20 {
        format!(" ({}/100)", len)
    } else {
        String::new()
    };
    let text = format!(
        "\n  表示名: {}_{}\n\n  Enter: 確定  Esc: キャンセル\n  (空で確定すると元の名前に戻ります)",
        app.rename_project_input, counter,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_add_worktree_popup(frame: &mut Frame, app: &App) {
    use crate::app::AddWorktreeMode;

    let area = centered_rect(60, 50, frame.area());
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

    // モードタブを構築
    let modes = [
        (AddWorktreeMode::NewBranch, "新規"),
        (AddWorktreeMode::FromBase, "ベース"),
        (AddWorktreeMode::FromRemote, "リモート"),
    ];
    let mode_tabs: String = modes
        .iter()
        .map(|(mode, label)| {
            if *mode == app.add_worktree_mode {
                format!("[{}]", label)
            } else {
                format!(" {} ", label)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(format!("  名前: {}", app.add_worktree_name)),
        Line::from(""),
        Line::from(format!("  モード: {}", mode_tabs)),
        Line::from(""),
    ];

    // フォーカス中のフィールドにのみカーソル `_` を表示
    let branch_cursor = if app.add_worktree_display_focus { "" } else { "_" };

    // 表示名行: 未入力時は placeholder を薄く表示し、入力があれば消える
    let display_name_line = {
        let mut spans = vec![Span::raw("  表示名: ")];
        if app.add_worktree_display_input.is_empty() {
            // フォーカス時はカーソルのみ、非フォーカス時は placeholder のみ（併置を避ける）
            if app.add_worktree_display_focus {
                spans.push(Span::raw("_"));
            } else {
                spans.push(Span::styled(
                    "(任意)",
                    Style::default().fg(Color::DarkGray),
                ));
            }
        } else {
            spans.push(Span::raw(app.add_worktree_display_input.clone()));
            if app.add_worktree_display_focus {
                spans.push(Span::raw("_"));
            }
        }
        Line::from(spans)
    };

    match app.add_worktree_mode {
        AddWorktreeMode::NewBranch => {
            // ブランチ一覧が下に並ぶため、表示名はブランチ名の上に置く
            lines.push(display_name_line);
            lines.push(Line::from(format!(
                "  ブランチ: {}{}",
                app.add_worktree_input, branch_cursor
            )));
        }
        AddWorktreeMode::FromBase => {
            // 選択中のベースブランチを表示
            let selected_base = app
                .add_worktree_all_branches
                .get(app.add_worktree_base_cursor)
                .map(|s| s.as_str())
                .unwrap_or(&app.add_worktree_base_branch);
            lines.push(Line::from(format!("  ベース: {}", selected_base)));
            // ブランチ一覧が下に並ぶため、表示名はブランチ名の上に置く
            lines.push(display_name_line);
            lines.push(Line::from(format!(
                "  ブランチ: {}{}",
                app.add_worktree_input, branch_cursor
            )));

            if app.add_worktree_base_loading {
                lines.push(Line::from(""));
                lines.push(Line::from("  取得中..."));
            } else if !app.add_worktree_all_branches.is_empty() {
                lines.push(Line::from(""));

                let branches = &app.add_worktree_all_branches;
                let max_display = (area.height as usize).saturating_sub(lines.len() + 4);
                let start = if app.add_worktree_base_cursor >= max_display {
                    app.add_worktree_base_cursor - max_display + 1
                } else {
                    0
                };

                for (i, branch) in branches.iter().enumerate().skip(start).take(max_display) {
                    let marker = if i == app.add_worktree_base_cursor {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(Line::from(format!("  {} {}", marker, branch)));
                }
            }
        }
        AddWorktreeMode::FromRemote => {
            // FromRemote は既存リモートブランチのチェックアウトのため表示名入力を提供しない
            // （display_name_line は push せず、footer にも Shift+Tab: 表示名 を出さない）
            if app.add_worktree_loading {
                lines.push(Line::from("  取得中..."));
            } else {
                lines.push(Line::from(format!(
                    "  フィルター: {}_",
                    app.add_worktree_branch_filter
                )));
                lines.push(Line::from(""));

                let filtered: Vec<&String> = app
                    .add_worktree_remote_branches
                    .iter()
                    .filter(|b| {
                        app.add_worktree_branch_filter.is_empty()
                            || b.contains(&app.add_worktree_branch_filter)
                    })
                    .collect();

                // 表示可能な行数を計算（ポップアップ内の残りスペース）
                let max_display = (area.height as usize).saturating_sub(lines.len() + 4);
                let start = if app.add_worktree_branch_cursor >= max_display {
                    app.add_worktree_branch_cursor - max_display + 1
                } else {
                    0
                };

                for (i, branch) in filtered.iter().enumerate().skip(start).take(max_display) {
                    let marker = if i == app.add_worktree_branch_cursor {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(Line::from(format!("  {} {}", marker, branch)));
                }

                if filtered.is_empty() {
                    lines.push(Line::from("  (リモートブランチなし)"));
                }
            }
        }
    }

    lines.push(Line::from(""));
    let footer = match app.add_worktree_mode {
        AddWorktreeMode::NewBranch => {
            "  Enter: 追加  Shift+Tab: 表示名  Tab: モード  Esc: 閉じる"
        }
        AddWorktreeMode::FromBase => {
            "  Enter: 追加  ↑↓: ベース選択  Shift+Tab: 表示名  Tab: モード  Esc: 閉じる"
        }
        AddWorktreeMode::FromRemote => {
            "  Enter: 追加  ↑↓: 選択  文字: フィルタ  Tab: モード  Esc: 閉じる"
        }
    };
    lines.push(Line::from(footer));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_grep_popup(frame: &mut Frame, app: &App) {
    let w = 50_u16.min(frame.area().width);
    let h = 3_u16;
    let x = (frame.area().width.saturating_sub(w)) / 2;
    let y = (frame.area().height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Search (Enter で検索)")
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(ratatui::widgets::Clear, area);

    let input_text = format!(" {}_", app.grep_input);
    frame.render_widget(
        Paragraph::new(input_text)
            .block(block)
            .style(Style::default().fg(Color::Yellow)),
        area,
    );
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

fn render_remove_project_confirm_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(40, 20, frame.area());

    let project_name = app
        .remove_project_target
        .and_then(|pi| app.projects.get(pi))
        .map(|p| p.name.as_str())
        .unwrap_or("???");

    let block = Block::default()
        .borders(Borders::ALL)
        .title("プロジェクト除外")
        .border_style(Style::default().fg(Color::Red));

    let text = format!(
        "\n  \"{}\" をリストから除外しますか？\n  (プロジェクトファイルは削除されません)\n\n  y: 除外  n: キャンセル",
        project_name,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_siki_json_confirm_popup(frame: &mut Frame) {
    let area = centered_rect(40, 20, frame.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .title("siki.json")
        .border_style(Style::default().fg(Color::Yellow));

    let text = "\n  siki.json が見つかりません。\n  Claude で対話的に作成しますか？\n\n  y: 作成  n: スキップ";
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_siki_json_init_terminal(
    frame: &mut Frame,
    screen: Option<&vt100::Screen>,
    scroll_offset: usize,
    spinner: usize,
) {
    let area = centered_rect(90, 80, frame.area());

    let spinner_chars = ['|', '/', '-', '\\'];
    let spinner_char = spinner_chars[spinner % spinner_chars.len()];

    let title = if scroll_offset > 0 {
        format!(
            "siki.json 作成 [↑{}行] (Scroll: ホイール/Shift+PgUp,PgDn  Esc で閉じる)",
            scroll_offset
        )
    } else {
        "siki.json 作成 (Scroll: ホイール/Shift+PgUp,PgDn  Esc で閉じる)".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    frame.render_widget(ratatui::widgets::Clear, area);

    let is_loading = match screen {
        Some(screen) => screen.contents().trim().is_empty(),
        None => true,
    };

    if is_loading {
        let loading_text = format!("\n\n  {} Claude を起動中...", spinner_char);
        frame.render_widget(
            Paragraph::new(loading_text)
                .block(block)
                .style(Style::default().fg(Color::Yellow)),
            area,
        );
    } else if let Some(screen) = screen {
        let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);
        frame.render_widget(pseudo_term, area);
    } else {
        // unreachable だが念のため
        frame.render_widget(
            Paragraph::new("\n\n  Claude を起動中...")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }
}

fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
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

    // コンテンツ領域を計算して保存（選択座標変換に使用）
    let content_area = block.inner(area);
    app.terminal_content_area = Some(content_area);

    match terminal_screen {
        Some(screen) => {
            let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);
            frame.render_widget(pseudo_term, area);

            // 選択範囲のハイライト描画
            if let Some(ref sel) = app.text_selection {
                if sel.panel == SelectionPanel::Terminal {
                    let current_scroll = screen.scrollback();
                    let max_row = content_area.height.saturating_sub(1);
                    if let Some((start, end)) = sel.adjusted_normalize(current_scroll, max_row) {
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
                            let fg = cell.fg;
                            let bg = cell.bg;
                            cell.set_fg(if bg == Color::Reset { Color::Black } else { bg });
                            cell.set_bg(if fg == Color::Reset { Color::White } else { fg });
                        }
                    }
                    }
                }
            }
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

fn render_skill_name_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, frame.area());
    let project = app.skill_project_name.as_deref().unwrap_or("?");
    let title = format!("Skill 作成: {}", project);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    let text = format!(
        "\n  スキル名: {}_\n\n  Enter: 作成  Esc: キャンセル",
        app.skill_name_input,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_skill_edit_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(80, 80, frame.area());
    let skill_name = &app.skill_name_input;
    let title = format!(
        "Skill: /{} (Enter: 保存  Shift+Enter: 改行  Ctrl+R: Claude整形  Esc: キャンセル)",
        skill_name
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(ratatui::widgets::Clear, area);

    let s = &app.skill_content_input;
    let cur = app.skill_content_cursor;

    // カーソル位置で before / cursor_char / after に分割
    let (before_str, cursor_str, after_str) = if cur >= s.len() {
        (s.as_str(), " ", "")
    } else {
        let ch_len = s[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        (&s[..cur], &s[cur..cur + ch_len], &s[cur + ch_len..])
    };

    // 改行を含むテキストを行ごとに分割して Line のリストを構築
    let cursor_style = Style::default().bg(Color::White).fg(Color::Black);
    let full = format!("{}\x00{}", before_str, after_str); // \x00 をカーソル位置マーカーに
    let _ = full; // unused

    // before の行数でカーソルの行を特定
    let before_lines: Vec<&str> = before_str.split('\n').collect();
    let after_lines: Vec<&str> = after_str.split('\n').collect();
    let cursor_line_idx = before_lines.len() - 1;

    // 全体を行ごとに構築
    let mut lines: Vec<Line> = Vec::new();

    // カーソル前の行（カーソル行より前）
    for line_str in &before_lines[..cursor_line_idx] {
        lines.push(Line::from(*line_str));
    }

    // カーソル行: before の最後の部分 + カーソル文字 + after の最初の部分
    let cursor_line_before = before_lines[cursor_line_idx];
    let cursor_line_after = after_lines.first().copied().unwrap_or("");
    let cursor_display = if cursor_str == "\n" { " " } else { cursor_str };
    lines.push(Line::from(vec![
        Span::raw(cursor_line_before),
        Span::styled(cursor_display, cursor_style),
        Span::raw(cursor_line_after),
    ]));

    // カーソル後の行（after の2行目以降）
    for line_str in after_lines.iter().skip(1) {
        lines.push(Line::from(*line_str));
    }

    // カーソルが改行文字上にある場合、afterの最初の行は次の行に入るので追加行は不要

    // 整形中はスピナーを左下に表示
    if app.skill_refining {
        let spinner_chars = ['|', '/', '-', '\\'];
        let spinner = spinner_chars[app.skill_refine_spinner % spinner_chars.len()];
        let block = block.title_bottom(format!(" {} Claude で整形中... ", spinner));

        let text = ratatui::text::Text::from(lines)
            .style(Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(paragraph, area);
    } else {
        let text = ratatui::text::Text::from(lines);
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

fn render_skill_list_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(80, 80, frame.area());

    let project = app.skill_project_name.as_deref().unwrap_or("?");
    let title = format!("Skills: {}", project);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(" n: 新規  Enter: 編集  d: 削除  Esc: 閉じる ")
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(ratatui::widgets::Clear, area);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.skill_list_items.is_empty() {
        frame.render_widget(
            Paragraph::new("\n  スキルがありません\n\n  n キーで新規作成").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    // 左: スキル名リスト (幅24), 右: 内容プレビュー
    let chunks = Layout::horizontal([
        Constraint::Length(24),
        Constraint::Min(0),
    ])
    .split(inner);

    // 左パネル: スキル名リスト
    let list_items: Vec<ratatui::widgets::ListItem> = app
        .skill_list_items
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let style = if i == app.skill_list_cursor {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ratatui::widgets::ListItem::new(format!("  /{}", name)).style(style)
        })
        .collect();
    let list = ratatui::widgets::List::new(list_items);
    frame.render_widget(list, chunks[0]);

    // 右パネル: 選択中のスキルの内容
    if let Some((_, content)) = app.skill_list_items.get(app.skill_list_cursor) {
        let preview = Paragraph::new(content.as_str())
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().fg(Color::Gray));
        frame.render_widget(preview, chunks[1]);
    }
}

fn render_context_list_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(80, 80, frame.area());

    let wt = app.context_worktree_name.as_deref().unwrap_or("?");
    let title = format!("Contexts: {}", wt);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(" n: テキスト追加  u: URL追加  Enter: 編集  d: 削除  Esc: 閉じる ")
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(ratatui::widgets::Clear, area);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.context_list_items.is_empty() {
        frame.render_widget(
            Paragraph::new("\n  コンテキストがありません\n\n  n: テキスト追加  u: URL追加")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    // 左: コンテキスト名リスト (幅24), 右: 内容プレビュー
    let chunks = Layout::horizontal([Constraint::Length(24), Constraint::Min(0)]).split(inner);

    let list_items: Vec<ratatui::widgets::ListItem> = app
        .context_list_items
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let style = if i == app.context_list_cursor {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ratatui::widgets::ListItem::new(format!("  {}", name)).style(style)
        })
        .collect();
    let list = ratatui::widgets::List::new(list_items);
    frame.render_widget(list, chunks[0]);

    if let Some((_, content)) = app.context_list_items.get(app.context_list_cursor) {
        let preview = Paragraph::new(content.as_str())
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().fg(Color::Gray));
        frame.render_widget(preview, chunks[1]);
    }
}

fn render_context_name_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, frame.area());
    let mode_label = match app.context_add_mode {
        crate::app::ContextAddMode::Text => "テキスト",
        crate::app::ContextAddMode::Url => "URL",
    };
    let wt = app.context_worktree_name.as_deref().unwrap_or("?");
    let title = format!("Context 追加 ({}): {}", mode_label, wt);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    let text = format!(
        "\n  コンテキスト名: {}_\n\n  Enter: 作成  Esc: キャンセル",
        app.context_name_input,
    );
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_context_edit_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(80, 80, frame.area());
    let name = &app.context_name_input;
    let total_lines = app.context_content_input.lines().count() + 1;
    let title = format!(
        "Context: {} (Enter: 保存  Shift+Enter: 改行  Ctrl+R: Claude整形  PgUp/PgDn: スクロール)",
        name
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(ratatui::widgets::Clear, area);

    let inner_height = block.inner(area).height as usize;

    let s = &app.context_content_input;
    let cur = app.context_content_cursor;

    let (before_str, cursor_str, after_str) = if cur >= s.len() {
        (s.as_str(), " ", "")
    } else {
        let ch_len = s[cur..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        (&s[..cur], &s[cur..cur + ch_len], &s[cur + ch_len..])
    };

    let cursor_style = Style::default().bg(Color::White).fg(Color::Black);
    let sel_style = Style::default().bg(Color::Blue).fg(Color::White);

    // 選択範囲の正規化
    let selection = app.context_edit_selection.and_then(|(a, b)| {
        if a == b { None } else { Some((a.min(b), a.max(b))) }
    });

    let mut lines: Vec<Line> = Vec::new();
    let cursor_line_idx;

    if let Some((sel_start, sel_end)) = selection {
        // 選択範囲ありの場合: 行ごとに通常/選択/カーソルを描画
        let mut byte_pos = 0;
        let text_lines: Vec<&str> = s.split('\n').collect();
        cursor_line_idx = s[..cur].matches('\n').count();

        for (line_idx, line_text) in text_lines.iter().enumerate() {
            let line_start = byte_pos;
            let line_end = byte_pos + line_text.len();

            // この行と選択範囲の交差を算出
            let sel_s = sel_start.max(line_start).min(line_end);
            let sel_e = sel_end.max(line_start).min(line_end);

            if sel_s >= sel_e {
                // 選択がこの行にかからない
                if line_idx == cursor_line_idx && cur >= line_start && cur <= line_end {
                    // カーソル行
                    let c_off = cur - line_start;
                    if c_off >= line_text.len() {
                        lines.push(Line::from(vec![
                            Span::raw(*line_text),
                            Span::styled(" ", cursor_style),
                        ]));
                    } else {
                        let ch_len = line_text[c_off..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                        lines.push(Line::from(vec![
                            Span::raw(&line_text[..c_off]),
                            Span::styled(&line_text[c_off..c_off + ch_len], cursor_style),
                            Span::raw(&line_text[c_off + ch_len..]),
                        ]));
                    }
                } else {
                    lines.push(Line::from(*line_text));
                }
            } else {
                // 選択がこの行にかかる
                let before = &line_text[..sel_s - line_start];
                let selected = &line_text[sel_s - line_start..sel_e - line_start];
                let after = &line_text[sel_e - line_start..];
                lines.push(Line::from(vec![
                    Span::raw(before),
                    Span::styled(selected, sel_style),
                    Span::raw(after),
                ]));
            }

            byte_pos = line_end + 1; // +1 for '\n'
        }
    } else {
        // 選択なし: カーソルのみ表示
        let before_lines: Vec<&str> = before_str.split('\n').collect();
        let after_lines: Vec<&str> = after_str.split('\n').collect();
        cursor_line_idx = before_lines.len() - 1;

        for line_str in &before_lines[..cursor_line_idx] {
            lines.push(Line::from(*line_str));
        }

        let cursor_line_before = before_lines[cursor_line_idx];
        if cursor_str == "\n" {
            lines.push(Line::from(vec![
                Span::raw(cursor_line_before),
                Span::styled(" ", cursor_style),
            ]));
            for line_str in &after_lines {
                lines.push(Line::from(*line_str));
            }
        } else {
            let cursor_line_after = after_lines.first().copied().unwrap_or("");
            lines.push(Line::from(vec![
                Span::raw(cursor_line_before),
                Span::styled(cursor_str, cursor_style),
                Span::raw(cursor_line_after),
            ]));
            for line_str in after_lines.iter().skip(1) {
                lines.push(Line::from(*line_str));
            }
        }
    }

    // スクロールオフセットを適用
    let scroll = app.context_edit_scroll.min(total_lines.saturating_sub(inner_height));

    // フッター: 行数情報
    let bottom_info = format!(" {}/{} lines  Esc: キャンセル ", cursor_line_idx + 1, total_lines);

    if app.context_refining {
        let spinner_chars = ['|', '/', '-', '\\'];
        let spinner = spinner_chars[app.context_refine_spinner % spinner_chars.len()];
        let block = block
            .title_bottom(format!(" {} Claude で整形中... ", spinner));

        let text = ratatui::text::Text::from(lines)
            .style(Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(text)
            .block(block)
            .scroll((scroll as u16, 0));
        frame.render_widget(paragraph, area);
    } else {
        let block = block.title_bottom(bottom_info);
        let text = ratatui::text::Text::from(lines);
        let paragraph = Paragraph::new(text)
            .block(block)
            .scroll((scroll as u16, 0));
        frame.render_widget(paragraph, area);
    }
}

fn render_context_url_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 20, frame.area());
    let name = &app.context_name_input;
    let title = format!("URL 入力: {}", name);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Green));

    if app.context_url_fetching {
        let spinner_chars = ['|', '/', '-', '\\'];
        let spinner = spinner_chars[app.context_url_spinner % spinner_chars.len()];
        let text = format!("\n  {} URL からコンテンツを取得中...", spinner);
        let block = block.title_bottom(" 取得・要約中 ");
        let paragraph = Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(ratatui::widgets::Clear, area);
        frame.render_widget(paragraph, area);
    } else {
        let text = format!(
            "\n  URL: {}_\n\n  Enter: 取得  Esc: キャンセル",
            app.context_url_input,
        );
        let paragraph = Paragraph::new(text).block(block);
        frame.render_widget(ratatui::widgets::Clear, area);
        frame.render_widget(paragraph, area);
    }
}

fn render_symlink_settings_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 60, frame.area());

    let project = app.symlink_project_name.as_deref().unwrap_or("?");
    let title = format!("Symlinks: {}", project);
    let bottom = if app.symlink_input_mode {
        " Enter: 追加  Esc: キャンセル "
    } else {
        " Space: 切替  a: 追加  d: 削除  Enter: 保存  Esc: 戻る "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(bottom)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(ratatui::widgets::Clear, area);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.symlink_items.is_empty() && !app.symlink_input_mode {
        frame.render_widget(
            Paragraph::new("\n  候補が見つかりません\n\n  a キーで手動追加")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    // 入力モード用に下部を確保
    let (list_area, input_area) = if app.symlink_input_mode {
        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    // チェックボックスリスト
    let list_items: Vec<ratatui::widgets::ListItem> = app
        .symlink_items
        .iter()
        .enumerate()
        .map(|(i, (name, enabled))| {
            let check = if *enabled { "[x]" } else { "[ ]" };
            let style = if i == app.symlink_cursor {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if *enabled {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Gray)
            };
            ratatui::widgets::ListItem::new(format!("  {} {}", check, name)).style(style)
        })
        .collect();
    let list = ratatui::widgets::List::new(list_items);
    frame.render_widget(list, list_area);

    // 入力行
    if let Some(input_area) = input_area {
        let input_text = format!("  > {}_ ", app.symlink_input);
        frame.render_widget(
            Paragraph::new(input_text).style(Style::default().fg(Color::Yellow)),
            input_area,
        );
    }
}

/// エージェント監視ビューの 1 行分の表示データ。
///
/// プロジェクト別ポップアップ（TASK-0006）と全体ダッシュボード（TASK-0007）で共有する。
/// `SessionRegistry` のライブ参照からクローンで構築し、`Session` はミューテートしない。
/// `summary` はインメモリ Registry が保持しないため現状は常に `None`（DB 由来の summary は
/// UI のライブ参照経路には乗らない）。
pub struct AgentRow {
    pub project_name: String,
    pub worktree_name: String,
    pub role: String,
    pub state: SessionState,
    pub activity: Option<String>,
    pub summary: Option<String>,
    pub elapsed_secs: u64,
    pub alert: bool,
}

/// 経過秒数を人間可読の短い文字列（`12s` / `3m` / `2h`）に整形する（REQ-005）。
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// 人間の対応が必要な行か（alert あり、または許可入力待ち）を判定する（REQ-104）。
fn is_attention(row: &AgentRow) -> bool {
    row.alert || matches!(row.state, SessionState::Waiting)
}

/// 文字列を最大 `max` 文字に収め、超過時は末尾を `…` で省略する（EDGE-102）。
///
/// 文字（char）単位で数えるためマルチバイト文字でもパニックしない。
fn truncate_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let head: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{head}…")
}

/// 対象プロジェクト配下のエージェント活動をポップアップ表示する（REQ-003）。
///
/// `app.agent_popup_project_index` のプロジェクトに属する全 worktree のライブセッションを
/// `registry.by_worktree` で取得し、role / 状態 / activity / 経過時間 / alert を 1 行ずつ表示する。
/// alert または Waiting の行は赤系で強調（REQ-104）、長文は `…` で省略（EDGE-102）、
/// `agent_popup_scroll` でスクロール（REQ-301）、対象が無ければ空表示（REQ-202）。
///
/// TASK-0008 で `m` キーの render 分岐から呼ばれる。
#[allow(dead_code)]
pub fn render_agent_popup(frame: &mut Frame, app: &App, registry: &SessionRegistry) {
    let area = centered_rect(60, 50, frame.area());
    frame.render_widget(ratatui::widgets::Clear, area);

    let project_name = app
        .projects
        .get(app.agent_popup_project_index)
        .map(|p| p.name.as_str())
        .unwrap_or("???");

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Agents: {} ", project_name))
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 対象プロジェクトの worktree からライブセッションを AgentRow に整形
    let now = std::time::Instant::now();
    let mut rows: Vec<AgentRow> = Vec::new();
    if let Some(project) = app.projects.get(app.agent_popup_project_index) {
        for wt in &project.worktrees {
            for s in registry.by_worktree(&project.name, &wt.name) {
                rows.push(AgentRow {
                    project_name: s.project_name.clone(),
                    worktree_name: s.worktree_name.clone(),
                    role: s.role.clone(),
                    state: s.state,
                    activity: s.activity.clone(),
                    summary: None,
                    elapsed_secs: now.saturating_duration_since(s.last_seen).as_secs(),
                    alert: s.alert,
                });
            }
        }
    }

    // 空表示（REQ-202）
    if rows.is_empty() {
        let p = Paragraph::new("アクティブなエージェントなし").alignment(Alignment::Center);
        frame.render_widget(p, inner);
        return;
    }

    let lines = agent_rows_to_lines(&rows, inner.width as usize, false);

    // スクロール窓（REQ-301）
    let visible = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let start = app.agent_popup_scroll.min(max_scroll);
    let window: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();

    frame.render_widget(Paragraph::new(window), inner);
}

/// ダッシュボードのソートキー: 状態優先度の降順 → プロジェクト名 → worktree 名（REQ-006）。
fn dashboard_sort_key(row: &AgentRow) -> (std::cmp::Reverse<u8>, String, String) {
    (
        std::cmp::Reverse(row.state.priority()),
        row.project_name.clone(),
        row.worktree_name.clone(),
    )
}

/// 全セッションを `AgentRow` 化し状態優先順にソートして返す（unknown も除外しない, EDGE-003）。
fn build_dashboard_rows(registry: &SessionRegistry) -> Vec<AgentRow> {
    let now = std::time::Instant::now();
    let mut rows: Vec<AgentRow> = registry
        .all()
        .map(|s| AgentRow {
            project_name: s.project_name.clone(),
            worktree_name: s.worktree_name.clone(),
            role: s.role.clone(),
            state: s.state,
            activity: s.activity.clone(),
            summary: None,
            elapsed_secs: now.saturating_duration_since(s.last_seen).as_secs(),
            alert: s.alert,
        })
        .collect();
    rows.sort_by_key(dashboard_sort_key);
    rows
}

/// 全プロジェクト横断のエージェント活動をダッシュボード表示する（REQ-004）。
///
/// `registry.all()` の全セッション（unknown/unknown 含む, EDGE-003）を状態優先順
/// （Waiting→Working→Done→Idle, 同状態は project→worktree 名順, REQ-006）に並べ、
/// 赤強調・省略・経過時間・スクロールは TASK-0006 の共通ヘルパで描画する。
///
/// TASK-0008 で `M` キーの render 分岐から呼ばれる。
#[allow(dead_code)]
pub fn render_agent_dashboard(frame: &mut Frame, app: &App, registry: &SessionRegistry) {
    let area = centered_rect(80, 80, frame.area());
    frame.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Agents: all ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = build_dashboard_rows(registry);

    if rows.is_empty() {
        let p = Paragraph::new("アクティブなエージェントなし").alignment(Alignment::Center);
        frame.render_widget(p, inner);
        return;
    }

    let lines = agent_rows_to_lines(&rows, inner.width as usize, true);

    let visible = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let start = app.agent_dashboard_scroll.min(max_scroll);
    let window: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();

    frame.render_widget(Paragraph::new(window), inner);
}

/// AgentRow 群を描画用の `Line` 群へ整形する（赤強調・省略を適用）。
///
/// `width` は描画可能な内側の幅（文字数の目安）。ポップアップ／ダッシュボード共通。
/// `show_project` が true のときはラベルに `project/` を前置する（ダッシュボード用）。
fn agent_rows_to_lines(rows: &[AgentRow], width: usize, show_project: bool) -> Vec<Line<'static>> {
    rows.iter()
        .map(|row| {
            let badge = row.state.badge_char(true);
            // 「[project/]worktree(role)」のラベル部
            let label = if show_project {
                format!("{} {}/{}({})", badge, row.project_name, row.worktree_name, row.role)
            } else {
                format!("{} {}({})", badge, row.worktree_name, row.role)
            };
            let elapsed = format_elapsed(row.elapsed_secs);
            // 活動内容（None は "-"）
            let activity = row.activity.as_deref().unwrap_or("-");
            // 残り幅に応じて活動内容を省略（ラベル・経過時間・区切りを差し引く）
            let reserved = label.chars().count() + elapsed.chars().count() + 4;
            let avail = width.saturating_sub(reserved);
            let activity = truncate_ellipsis(activity, avail);
            let text = format!("{label}  {activity}  {elapsed}");

            let style = if is_attention(row) {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(row.state.badge_color())
            };
            Line::from(Span::styled(text, style))
        })
        .collect()
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;

    fn row(state: SessionState, alert: bool) -> AgentRow {
        AgentRow {
            project_name: "proj".into(),
            worktree_name: "wt".into(),
            role: "main".into(),
            state,
            activity: None,
            summary: None,
            elapsed_secs: 0,
            alert,
        }
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(12), "12s");
        assert_eq!(format_elapsed(180), "3m");
        assert_eq!(format_elapsed(7200), "2h");
        // 境界値
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(3599), "59m");
        assert_eq!(format_elapsed(3600), "1h");
    }

    #[test]
    fn attention_for_alert_and_waiting() {
        assert!(is_attention(&row(SessionState::Working, true)));
        assert!(is_attention(&row(SessionState::Waiting, false)));
        assert!(!is_attention(&row(SessionState::Working, false)));
        assert!(!is_attention(&row(SessionState::Idle, false)));
        assert!(!is_attention(&row(SessionState::Done, false)));
    }

    #[test]
    fn truncate_adds_ellipsis() {
        assert_eq!(truncate_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_ellipsis("abcdef", 10), "abcdef");
        assert_eq!(truncate_ellipsis("abcdef", 6), "abcdef");
        assert_eq!(truncate_ellipsis("abcdef", 0), "");
        assert_eq!(truncate_ellipsis("abcdef", 1), "…");
        // マルチバイト（日本語）でもパニックしない
        assert_eq!(truncate_ellipsis("あいうえお", 3), "あい…");
    }

    fn named_row(state: SessionState, project: &str, worktree: &str) -> AgentRow {
        AgentRow {
            project_name: project.into(),
            worktree_name: worktree.into(),
            role: "main".into(),
            state,
            activity: None,
            summary: None,
            elapsed_secs: 0,
            alert: false,
        }
    }

    #[test]
    fn sort_orders_by_state_then_names() {
        let mut rows = vec![
            named_row(SessionState::Idle, "z", "a"),
            named_row(SessionState::Working, "b", "y"),
            named_row(SessionState::Waiting, "m", "n"),
            named_row(SessionState::Done, "a", "a"),
            named_row(SessionState::Working, "b", "x"),
            named_row(SessionState::Working, "a", "a"),
        ];
        rows.sort_by_key(dashboard_sort_key);
        let order: Vec<(SessionState, &str, &str)> = rows
            .iter()
            .map(|r| (r.state, r.project_name.as_str(), r.worktree_name.as_str()))
            .collect();
        // waiting → working(同状態は project 昇順→worktree 昇順) → done → idle
        assert_eq!(
            order,
            vec![
                (SessionState::Waiting, "m", "n"),
                (SessionState::Working, "a", "a"),
                (SessionState::Working, "b", "x"),
                (SessionState::Working, "b", "y"),
                (SessionState::Done, "a", "a"),
                (SessionState::Idle, "z", "a"),
            ]
        );
    }

    #[test]
    fn unknown_rows_not_filtered_out() {
        let mut registry = SessionRegistry::new();
        // cwd が workspaces 配下でないため unknown/unknown になる
        registry.register("sess-unknown".into(), "/tmp".into(), "main".into());
        let rows = build_dashboard_rows(&registry);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_name, "unknown");
        assert_eq!(rows[0].worktree_name, "unknown");
    }
}

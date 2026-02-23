mod app;
mod claude;
mod config;
mod event;
mod git;
mod terminal;
mod tui;
mod ui;

use anyhow::Result;
use config::load_config;
use std::collections::HashMap;
use ui::diff_view::DiffView;
use ui::left_panel::LeftPanel;
use ui::source_tree::SourceTree;

/// ターミナルエミュレータのキー: (worktree_id, tab_index)
type TerminalKey = (app::WorktreeId, usize);

/// Claude ターミナルを識別する特殊なタブインデックスの基底値
/// 実際のタブインデックス = CLAUDE_TAB_BASE + claude_tab_index
const CLAUDE_TAB_BASE: usize = usize::MAX - 100;

#[tokio::main]
async fn main() -> Result<()> {
    tui::install_panic_hook();

    config::ensure_dirs()?;
    let config_path = config::default_config_path();
    let config = load_config(&config_path)?;
    let shell = config::resolve_shell(&config);
    let mut app = app::App::new(&config);
    let mut left_panel = LeftPanel::new();
    let mut source_tree = SourceTree::new();
    let mut diff_view = DiffView::new();
    let mut sessions: HashMap<app::WorktreeId, claude::ClaudeSession> = HashMap::new();
    let mut terminals: HashMap<TerminalKey, terminal::TerminalEmulator> = HashMap::new();
    let mut claude_terms: HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator> =
        HashMap::new();

    let mut tui_terminal = tui::init()?;
    let mut events = event::EventHandler::new();
    let event_tx = events.sender();
    let mut last_layout: Option<ui::layout::AppLayout> = None;

    // メインイベントループ
    while app.running {
        // 現在のターミナル画面とタブ情報を取得
        let (terminal_screen, terminal_tab_info) = if let Some(wt_id) = app.selected_worktree {
            let active = app
                .worktree_by_id(wt_id)
                .map(|wt| wt.active_terminal)
                .unwrap_or(0);

            let screen = terminals.get(&(wt_id, active)).map(|emu| emu.screen());

            let tabs: Vec<usize> = (0..5)
                .filter(|i| terminals.contains_key(&(wt_id, *i)))
                .collect();

            let tab_info = if !tabs.is_empty() {
                Some(ui::TerminalTabInfo {
                    tabs,
                    active,
                })
            } else {
                None
            };

            (screen, tab_info)
        } else {
            (None, None)
        };

        // Claude ターミナル画面を取得（active_tab が Claude タブの場合）
        let claude_screen = app.selected_worktree.and_then(|wt_id| {
            let tab = app.worktree_by_id(wt_id)?.active_tab;
            let claude_tabs = app.worktree_by_id(wt_id)?.claude_tabs;
            if tab < claude_tabs {
                claude_terms.get(&(wt_id, tab)).map(|emu| emu.screen())
            } else {
                None
            }
        });

        // UI 描画
        tui_terminal.draw(|frame| {
            last_layout = Some(ui::render(
                frame,
                &app,
                &left_panel,
                &source_tree,
                &diff_view,
                terminal_screen,
                terminal_tab_info.as_ref(),
                claude_screen,
            ));
        })?;

        // イベント待受と処理
        let ev = events.next().await?;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &event_tx,
            &shell,
            ev,
            last_layout.as_ref(),
        )
        .await;
    }

    // クリーンアップ: 全セッションを終了
    for (_id, mut session) in sessions.drain() {
        let _ = session.kill().await;
    }

    tui::restore()?;
    Ok(())
}

async fn handle_event(
    app: &mut app::App,
    left_panel: &mut LeftPanel,
    source_tree: &mut SourceTree,
    diff_view: &mut DiffView,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    event: event::AppEvent,
    last_layout: Option<&ui::layout::AppLayout>,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    use event::AppEvent;

    match event {
        AppEvent::Key(key) => {
            // ポップアップ表示中はポップアップ専用の処理
            if app.show_help {
                if key.code == KeyCode::Esc {
                    app.show_help = false;
                }
                return;
            }
            if app.show_message_popup {
                handle_popup_key(app, sessions, event_tx, key).await;
                return;
            }
            if app.show_add_worktree_popup {
                handle_add_worktree_popup_key(app, terminals, event_tx, shell, key);
                return;
            }
            if app.show_add_project_popup {
                handle_add_project_popup_key(app, key);
                return;
            }

            // Terminal パネルフォーカス中は特別処理
            if app.focused_panel == app::Panel::Terminal {
                handle_terminal_key(app, terminals, event_tx, shell, key);
                return;
            }

            // グローバルキー
            match key.code {
                KeyCode::Char('q') => app.running = false,
                KeyCode::Char('?') => app.show_help = true,
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    app.cycle_focus(true);
                }
                KeyCode::BackTab => {
                    app.cycle_focus(true);
                }
                KeyCode::Tab => {
                    if app.focused_panel == app::Panel::Main {
                        ui::main_panel::next_tab(app);
                    } else {
                        app.cycle_focus(false);
                    }
                }
                _ => {
                    // パネル固有のキー処理
                    match app.focused_panel {
                        app::Panel::Left => {
                            handle_left_panel_key(app, left_panel, source_tree, diff_view, terminals, claude_terms, event_tx, shell, key);
                        }
                        app::Panel::Main => {
                            // 現在のタブが Claude タブならキーを転送
                            let on_claude_tab = app
                                .selected_worktree()
                                .map(|wt| wt.active_tab < wt.claude_tabs)
                                .unwrap_or(false);
                            if on_claude_tab {
                                handle_claude_terminal_key(app, claude_terms, key);
                            } else {
                                handle_main_panel_key(app, claude_terms, event_tx, key);
                            }
                        }
                        app::Panel::Right => {
                            handle_right_panel_key(app, source_tree, diff_view, key);
                        }
                        app::Panel::Terminal => unreachable!(),
                    }
                }
            }
        }
        AppEvent::ClaudeOutput {
            worktree_id,
            event: stream_event,
        } => {
            app.handle_claude_output(worktree_id, stream_event);
        }
        AppEvent::ClaudeComplete { worktree_id } => {
            app.handle_claude_complete(worktree_id);
        }
        AppEvent::ClaudeError {
            worktree_id,
            error,
        } => {
            app.handle_claude_error(worktree_id, &error);
        }
        AppEvent::TerminalOutput {
            worktree_id,
            tab_index,
            data,
        } => {
            if tab_index >= CLAUDE_TAB_BASE {
                let claude_idx = tab_index - CLAUDE_TAB_BASE;
                if let Some(emu) = claude_terms.get_mut(&(worktree_id, claude_idx)) {
                    emu.process(&data);
                }
            } else if let Some(emu) = terminals.get_mut(&(worktree_id, tab_index)) {
                emu.process(&data);
            }
        }
        AppEvent::Mouse(mouse) => {
            use crossterm::event::MouseEventKind;
            // ポップアップ表示中はマウスクリック無視
            if app.show_help || app.show_message_popup || app.show_add_worktree_popup || app.show_add_project_popup {
                return;
            }
            if let MouseEventKind::Down(crossterm::event::MouseButton::Left) = mouse.kind {
                if let Some(layout) = last_layout {
                    if let Some(panel) = layout.hit_test(mouse.column, mouse.row) {
                        app.focused_panel = panel;
                    }
                }
            }
        }
        AppEvent::Resize(_w, _h) => {
            // ratatui が自動的にリサイズを処理する
        }
        AppEvent::Tick => {
            app.clear_expired_status();
        }
    }
}

async fn handle_popup_key(
    app: &mut app::App,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Esc => {
            app.show_message_popup = false;
            app.popup_input.clear();
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let message = app.popup_input.clone();
            app.popup_input.clear();
            app.show_message_popup = false;
            if !message.is_empty() {
                if let Some(wt_id) = app.selected_worktree {
                    // チャット履歴にユーザーメッセージを追加
                    if let Some(wt) = app.selected_worktree_mut() {
                        wt.chat_history.push(app::ChatMessage {
                            role: app::Role::User,
                            content: message.clone(),
                            timestamp: chrono::Utc::now(),
                        });
                        wt.chat_scroll_offset = usize::MAX;
                    }

                    // Claude セッションにメッセージを送信
                    send_to_claude(app, sessions, event_tx, wt_id, &message).await;
                }
            }
        }
        KeyCode::Char(c) => {
            app.popup_input.push(c);
        }
        KeyCode::Backspace => {
            app.popup_input.pop();
        }
        _ => {}
    }
}

fn handle_add_project_popup_key(app: &mut app::App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.show_add_project_popup = false;
            app.add_project_input.clear();
        }
        KeyCode::Enter => {
            let path_str = app.add_project_input.trim().to_string();
            if path_str.is_empty() {
                return;
            }

            let path = std::path::PathBuf::from(&path_str);
            if !path.is_dir() {
                app.show_error(format!("ディレクトリが存在しません: {}", path_str));
                app.show_add_project_popup = false;
                app.add_project_input.clear();
                return;
            }

            // ディレクトリ名からプロジェクト名を自動生成
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path_str.clone());

            // 重複チェック
            if app.projects.iter().any(|p| p.name == name) {
                app.show_error(format!("同名のプロジェクトが既に存在します: {}", name));
                app.show_add_project_popup = false;
                app.add_project_input.clear();
                return;
            }

            app.projects.push(app::Project {
                name: name.clone(),
                path,
                worktrees: Vec::new(),
                collapsed: false,
            });

            // config.toml に保存
            let config_path = config::default_config_path();
            let config = build_config_from_app(app);
            if let Err(e) = config::save_config(&config_path, &config) {
                app.show_error(format!("設定の保存に失敗: {}", e));
            } else {
                app.show_info(format!("プロジェクト追加完了: {}", name));
            }

            app.show_add_project_popup = false;
            app.add_project_input.clear();
        }
        KeyCode::Char(c) => {
            app.add_project_input.push(c);
        }
        KeyCode::Backspace => {
            app.add_project_input.pop();
        }
        _ => {}
    }
}

fn handle_add_worktree_popup_key(
    app: &mut app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.show_add_worktree_popup = false;
            app.add_worktree_input.clear();
        }
        KeyCode::Enter => {
            let branch = app.add_worktree_input.trim().to_string();
            if branch.is_empty() {
                return;
            }
            let pi = app.add_worktree_project_index;
            let wt_name = app.add_worktree_name.clone();

            // メモリ上に worktree を追加
            let project_name = app.projects[pi].name.clone();
            let wt_path = config::worktree_path(&project_name, &wt_name);
            app.projects[pi].worktrees.push(app::Worktree {
                name: wt_name.clone(),
                branch: branch.clone(),
                path: wt_path.clone(),
                status: app::WorktreeStatus::Idle,
                chat_history: Vec::new(),
                open_files: Vec::new(),
                active_tab: 0,
                claude_tabs: 0,
                right_panel_mode: app::RightPanelMode::Tree,
                active_terminal: 0,
                chat_scroll_offset: 0,
            });

            // config.toml に保存
            let config_path = config::default_config_path();
            let config = build_config_from_app(app);
            if let Err(e) = config::save_config(&config_path, &config) {
                app.show_error(format!("設定の保存に失敗: {}", e));
            } else {
                app.show_info(format!("worktree 追加完了: {} ({})", wt_name, branch));
            }

            // siki.json の setup スクリプトがあれば実行
            let project_path = app.projects[pi].path.clone();
            let wi = app.projects[pi].worktrees.len() - 1;
            let wt_id = (pi, wi);
            if let Some(siki_json) = config::load_siki_json(&project_path) {
                if let Some(ref setup_script) = siki_json.scripts.setup {
                    run_siki_script(
                        app, terminals, event_tx, shell,
                        wt_id, setup_script, &wt_name, &wt_path,
                    );
                }
            }

            app.show_add_worktree_popup = false;
            app.add_worktree_input.clear();
        }
        KeyCode::Char(c) => {
            app.add_worktree_input.push(c);
        }
        KeyCode::Backspace => {
            app.add_worktree_input.pop();
        }
        _ => {}
    }
}

/// App の現在の状態から Config を構築する（既存の siki 設定を保持）
fn build_config_from_app(app: &app::App) -> config::Config {
    // 既存の設定ファイルから siki セクションを読み込み保持する
    let config_path = config::default_config_path();
    let siki = config::load_config(&config_path)
        .map(|c| c.siki)
        .unwrap_or(config::SikiConfig {
            shell: None,
            shared_dirs: vec![],
        });

    config::Config {
        siki,
        projects: app
            .projects
            .iter()
            .map(|p| config::ProjectConfig {
                name: p.name.clone(),
                path: p.path.to_string_lossy().to_string(),
                worktrees: p
                    .worktrees
                    .iter()
                    .map(|wt| config::WorktreeConfig {
                        name: wt.name.clone(),
                        branch: wt.branch.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Claude Code セッションにメッセージを送信する
///
/// セッションが存在しない場合は新規に起動する。
async fn send_to_claude(
    app: &mut app::App,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    wt_id: app::WorktreeId,
    message: &str,
) {
    if let Some(session) = sessions.get_mut(&wt_id) {
        // 既存セッションにメッセージ送信
        if let Err(e) = session.send_message(message).await {
            app.show_error(format!("メッセージ送信失敗: {}", e));
        } else if let Some(wt) = app.worktree_by_id_mut(wt_id) {
            wt.status = app::WorktreeStatus::Running;
        }
    } else {
        // 新規セッションを起動
        let path = match app.worktree_by_id(wt_id) {
            Some(wt) => wt.path.clone(),
            None => return,
        };

        match claude::ClaudeSession::spawn(&path, event_tx.clone(), wt_id).await {
            Ok(mut session) => {
                if let Err(e) = session.send_message(message).await {
                    app.show_error(format!("メッセージ送信失敗: {}", e));
                } else if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.status = app::WorktreeStatus::Running;
                }
                sessions.insert(wt_id, session);
            }
            Err(e) => {
                app.show_error(format!("Claude Code 起動失敗: {}", e));
            }
        }
    }
}

fn handle_terminal_key(
    app: &mut app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let Some(wt_id) = app.selected_worktree else {
        // worktree 未選択時は Right パネルに戻す
        app.focused_panel = app::Panel::Right;
        return;
    };

    let active_tab = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.active_terminal)
        .unwrap_or(0);

    let has_terminal = terminals.contains_key(&(wt_id, active_tab));

    // Ctrl+\ でターミナルから離脱
    if key.code == KeyCode::Char('\\') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.focused_panel = app::Panel::Right;
        return;
    }

    if !has_terminal {
        // ターミナル未作成時: n で新規作成、それ以外は無視
        if key.code == KeyCode::Char('n') {
            spawn_terminal(app, terminals, event_tx, shell, wt_id, 0);
        }
        return;
    }

    // ターミナルタブ切替（Ctrl+1..5）
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c @ '1'..='5') = key.code {
            let tab = (c as usize) - ('1' as usize);
            if terminals.contains_key(&(wt_id, tab)) {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.active_terminal = tab;
                }
            }
            return;
        }
        // Ctrl+n で新規タブ（最大5つ）
        if key.code == KeyCode::Char('n') {
            let next_tab = (0..5).find(|i| !terminals.contains_key(&(wt_id, *i)));
            if let Some(tab) = next_tab {
                spawn_terminal(app, terminals, event_tx, shell, wt_id, tab);
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.active_terminal = tab;
                }
            }
            return;
        }
    }

    // その他のキーは PTY に転送
    let bytes = terminal::key_to_bytes(&key);
    if !bytes.is_empty() {
        if let Some(emu) = terminals.get_mut(&(wt_id, active_tab)) {
            let _ = emu.write(&bytes);
        }
    }
}

/// siki.json のスクリプトをターミナルで実行する
fn run_siki_script(
    app: &mut app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    wt_id: app::WorktreeId,
    script: &str,
    worktree_name: &str,
    worktree_path: &std::path::Path,
) {
    let (pi, _) = wt_id;
    let project_path = app.projects[pi].path.clone();

    let active = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.active_terminal)
        .unwrap_or(0);

    // ターミナルがなければ作成
    if !terminals.contains_key(&(wt_id, active)) {
        spawn_terminal(app, terminals, event_tx, shell, wt_id, active);
    }

    if let Some(emu) = terminals.get_mut(&(wt_id, active)) {
        let env_setup = format!(
            "export SIKI_PROJECT_PATH=\"{}\" SIKI_WORKTREE_PATH=\"{}\" SIKI_WORKTREE_NAME=\"{}\"\n",
            project_path.display(),
            worktree_path.display(),
            worktree_name,
        );
        let _ = emu.write(env_setup.as_bytes());
        let cmd = format!("{}\n", script);
        let _ = emu.write(cmd.as_bytes());
    }
}

/// ターミナルを新規に作成する
fn spawn_terminal(
    app: &mut app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    wt_id: app::WorktreeId,
    tab_index: usize,
) {
    let (pi, _) = wt_id;
    let path = app.projects[pi].path.clone();

    // デフォルトサイズ（描画時にリサイズされる）
    let size = (80, 24);

    match terminal::TerminalEmulator::new(shell, &path, size, event_tx.clone(), wt_id, tab_index) {
        Ok(emu) => {
            terminals.insert((wt_id, tab_index), emu);
            if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                wt.active_terminal = tab_index;
            }
        }
        Err(e) => {
            app.show_error(format!("ターミナル起動失敗: {}", e));
        }
    }
}

fn handle_left_panel_key(
    app: &mut app::App,
    left_panel: &mut LeftPanel,
    source_tree: &mut SourceTree,
    diff_view: &mut DiffView,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    let entries = LeftPanel::build_entries(&app.projects);

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            left_panel.move_down(entries.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            left_panel.move_up();
        }
        KeyCode::Char(' ') => {
            left_panel.toggle_collapse(app, &entries);
            let new_entries = LeftPanel::build_entries(&app.projects);
            left_panel.clamp_cursor(new_entries.len());
        }
        KeyCode::Char('a') => {
            // カーソル位置からプロジェクトインデックスを特定
            let project_index = match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Project { index }) => Some(*index),
                Some(ui::left_panel::ListEntry::Worktree { project_index, .. }) => {
                    Some(*project_index)
                }
                None => None,
            };
            if let Some(pi) = project_index {
                // 既存 worktree 名を収集して重複しない都市名を生成
                let existing_names: Vec<String> = app.projects[pi]
                    .worktrees
                    .iter()
                    .map(|wt| wt.name.clone())
                    .collect();
                let city_name = config::generate_worktree_name(&existing_names);
                app.add_worktree_project_index = pi;
                app.add_worktree_name = city_name;
                app.add_worktree_input.clear();
                app.show_add_worktree_popup = true;
            }
        }
        KeyCode::Char('r') => {
            // worktree 行にカーソルがある場合のみ run スクリプトを実行
            if let Some(ui::left_panel::ListEntry::Worktree { project_index, worktree_index }) =
                left_panel.current_entry(&entries)
            {
                let pi = *project_index;
                let wi = *worktree_index;
                let wt_id = (pi, wi);
                let project_path = app.projects[pi].path.clone();
                let wt_name = app.projects[pi].worktrees[wi].name.clone();
                let wt_path = app.projects[pi].worktrees[wi].path.clone();

                if let Some(siki_json) = config::load_siki_json(&project_path) {
                    if let Some(ref run_script) = siki_json.scripts.run {
                        run_siki_script(
                            app, terminals, event_tx, shell,
                            wt_id, run_script, &wt_name, &wt_path,
                        );
                        app.focused_panel = app::Panel::Terminal;
                    } else {
                        app.show_info("siki.json に run スクリプトが定義されていません".to_string());
                    }
                } else {
                    app.show_info("siki.json が見つかりません".to_string());
                }
            }
        }
        KeyCode::Char('A') => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            app.add_project_input = cwd;
            app.show_add_project_popup = true;
        }
        KeyCode::Enter => {
            if let Some(wt_id) = left_panel.select_worktree(&entries) {
                app.selected_worktree = Some(wt_id);
                app.focused_panel = app::Panel::Main;
                // プロジェクトの実パスからソースツリーと diff を読み込む
                let (pi, _) = wt_id;
                let project_path = app.projects[pi].path.clone();
                source_tree.load(&project_path);
                diff_view.load(&project_path);
                // Claude Code とターミナルを自動起動
                let has_claude = app
                    .worktree_by_id(wt_id)
                    .map(|wt| wt.claude_tabs > 0)
                    .unwrap_or(false);
                if !has_claude {
                    launch_claude(app, claude_terms, event_tx, wt_id);
                }
                if !terminals.contains_key(&(wt_id, 0)) {
                    spawn_terminal(app, terminals, event_tx, shell, wt_id, 0);
                }
            }
        }
        _ => {}
    }
}

fn handle_main_panel_key(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            ui::main_panel::scroll_down(app);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            ui::main_panel::scroll_up(app);
        }
        KeyCode::Char('w') => {
            ui::main_panel::close_current_tab(app);
        }
        KeyCode::Char('i') => {
            // 新しい Claude Code タブを追加
            if let Some(wt_id) = app.selected_worktree {
                launch_claude(app, claude_terms, event_tx, wt_id);
            }
        }
        _ => {}
    }
}

/// Claude Code を中央パネルで起動する
///
/// プロジェクトのパスで `claude` をインタラクティブに起動する。
fn launch_claude(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    wt_id: app::WorktreeId,
) {
    let (pi, _) = wt_id;
    let project_path = app.projects[pi].path.clone();
    let claude_idx = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.claude_tabs)
        .unwrap_or(0);
    let size = (80, 24);

    match terminal::TerminalEmulator::new(
        "claude",
        &project_path,
        size,
        event_tx.clone(),
        wt_id,
        CLAUDE_TAB_BASE + claude_idx,
    ) {
        Ok(emu) => {
            claude_terms.insert((wt_id, claude_idx), emu);
            if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                wt.active_tab = wt.claude_tabs;
                wt.claude_tabs += 1;
            }
        }
        Err(e) => {
            app.show_error(format!("Claude Code 起動失敗: {}", e));
        }
    }
}

/// Claude ターミナルへのキー入力処理
///
/// Ctrl+\ で通常の Main パネルに戻る。それ以外のキーは PTY に転送する。
fn handle_claude_terminal_key(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let Some(wt_id) = app.selected_worktree else {
        return;
    };

    let active_tab = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.active_tab)
        .unwrap_or(0);

    // Ctrl+\ で Claude ターミナルを閉じる
    if key.code == KeyCode::Char('\\') && key.modifiers.contains(KeyModifiers::CONTROL) {
        claude_terms.remove(&(wt_id, active_tab));
        // claude_tabs を減らしてタブを詰める
        if let Some(wt) = app.worktree_by_id_mut(wt_id) {
            if wt.claude_tabs > 0 {
                wt.claude_tabs -= 1;
                // 閉じたタブより後ろの Claude ターミナルのキーをシフト
                for i in active_tab..wt.claude_tabs {
                    if let Some(emu) = claude_terms.remove(&(wt_id, i + 1)) {
                        claude_terms.insert((wt_id, i), emu);
                    }
                }
                if wt.active_tab >= wt.claude_tabs && wt.claude_tabs > 0 {
                    wt.active_tab = wt.claude_tabs - 1;
                } else if wt.claude_tabs == 0 {
                    wt.active_tab = 0;
                }
            }
        }
        return;
    }

    // キー入力を PTY に転送
    let bytes = terminal::key_to_bytes(&key);
    if !bytes.is_empty() {
        if let Some(emu) = claude_terms.get_mut(&(wt_id, active_tab)) {
            let _ = emu.write(&bytes);
        }
    }
}

fn handle_right_panel_key(
    app: &mut app::App,
    source_tree: &mut SourceTree,
    diff_view: &mut DiffView,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    let mode = app
        .selected_worktree()
        .map(|wt| wt.right_panel_mode)
        .unwrap_or(app::RightPanelMode::Tree);

    match mode {
        app::RightPanelMode::Tree => {
            // 検索モード中のキー処理（既存キーバインドより先に判定）
            if source_tree.search_active {
                match key.code {
                    KeyCode::Esc => source_tree.search_cancel(),
                    KeyCode::Enter => source_tree.search_confirm(),
                    KeyCode::Char(c) => source_tree.search_push(c),
                    KeyCode::Backspace => source_tree.search_pop(),
                    _ => {}
                }
                return;
            }

            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    source_tree.move_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    source_tree.move_up();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    source_tree.expand();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    source_tree.collapse();
                }
                KeyCode::Enter => {
                    if source_tree.current_is_dir() {
                        source_tree.toggle();
                    } else if let Some(path) = source_tree.current_file_path() {
                        ui::main_panel::open_file_tab(app, path);
                    }
                }
                KeyCode::Char('/') => {
                    source_tree.search_start();
                }
                KeyCode::Char('n') => {
                    source_tree.next_match();
                }
                KeyCode::Char('N') => {
                    source_tree.prev_match();
                }
                KeyCode::Char('t') => {
                    ui::right_panel::toggle_mode(app);
                }
                _ => {}
            }
        }
        app::RightPanelMode::Diff => match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                diff_view.scroll_down();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                diff_view.scroll_up();
            }
            KeyCode::Char('t') => {
                ui::right_panel::toggle_mode(app);
            }
            _ => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProjectConfig, SikiConfig, WorktreeConfig};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn sample_config() -> Config {
        Config {
            siki: SikiConfig {
                shell: Some("/bin/sh".to_string()),
                shared_dirs: vec![],
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path: "/tmp/test-project".to_string(),
                worktrees: vec![WorktreeConfig {
                    name: "feature".to_string(),
                    branch: "feature/test".to_string(),
                }],
            }],
        }
    }

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> crossterm::event::KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl_key(c: char) -> crossterm::event::KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // --- グローバルキーの状態遷移テスト ---

    #[tokio::test]
    async fn test_key_q_quits() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(app.running);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
        )
        .await;
        assert!(!app.running);
    }

    #[tokio::test]
    async fn test_key_question_opens_help() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(!app.show_help);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('?'))),
            None,
        )
        .await;
        assert!(app.show_help);
    }

    #[tokio::test]
    async fn test_help_popup_esc_closes() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_help = true;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
        )
        .await;
        assert!(!app.show_help);
    }

    #[tokio::test]
    async fn test_help_popup_blocks_other_keys() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_help = true;
        // ヘルプ表示中に q を押しても終了しない
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
        )
        .await;
        assert!(app.running);
        assert!(app.show_help);
    }

    #[tokio::test]
    async fn test_tab_cycles_focus_from_left() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(app.focused_panel, app::Panel::Left);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Main);
    }

    #[tokio::test]
    async fn test_tab_on_main_panel_switches_tab_not_focus() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.focused_panel = app::Panel::Main;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
        )
        .await;
        // Main パネルでは Tab はタブ切替なのでフォーカスは変わらない
        assert_eq!(app.focused_panel, app::Panel::Main);
    }

    #[tokio::test]
    async fn test_backtab_cycles_focus_reverse() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(app.focused_panel, app::Panel::Left);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::BackTab)),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Terminal);
    }

    #[tokio::test]
    async fn test_shift_tab_cycles_focus_reverse() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(app.focused_panel, app::Panel::Left);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(shift_key(KeyCode::Tab)),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Terminal);
    }

    // --- ポップアップの状態遷移テスト ---

    #[tokio::test]
    async fn test_popup_esc_closes() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_message_popup = true;
        app.popup_input = "hello".to_string();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
        )
        .await;
        assert!(!app.show_message_popup);
        assert!(app.popup_input.is_empty());
    }

    #[tokio::test]
    async fn test_popup_char_input() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_message_popup = true;
        for c in "abc".chars() {
            handle_event(
                &mut app,
                &mut left_panel,
                &mut source_tree,
                &mut diff_view,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
            )
            .await;
        }
        assert_eq!(app.popup_input, "abc");
    }

    #[tokio::test]
    async fn test_popup_backspace() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_message_popup = true;
        app.popup_input = "ab".to_string();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
        )
        .await;
        assert_eq!(app.popup_input, "a");
    }

    #[tokio::test]
    async fn test_popup_blocks_global_keys() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_message_popup = true;
        // ポップアップ中に q は入力文字として扱われる
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
        )
        .await;
        assert!(app.running);
        assert_eq!(app.popup_input, "q");
    }

    // --- ターミナルパネルの状態遷移テスト ---

    #[tokio::test]
    async fn test_terminal_ctrl_backslash_exits() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.selected_worktree = Some((0, 0));
        app.focused_panel = app::Panel::Terminal;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(ctrl_key('\\')),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Right);
    }

    #[tokio::test]
    async fn test_terminal_no_worktree_goes_to_right() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // worktree 未選択で Terminal パネルにキーを送る
        app.focused_panel = app::Panel::Terminal;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Right);
    }

    // --- Tick イベントのテスト ---

    #[tokio::test]
    async fn test_tick_clears_expired_status() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_info("test".to_string());
        // タイムスタンプを過去に設定
        app.status_set_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(10));

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
        )
        .await;
        assert!(app.status_message.is_none());
    }

    #[tokio::test]
    async fn test_tick_keeps_fresh_status() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_info("fresh".to_string());

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
        )
        .await;
        assert!(app.status_message.is_some());
    }

    // --- Claude イベントの状態遷移テスト ---

    #[tokio::test]
    async fn test_claude_output_event_updates_chat() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeOutput {
                worktree_id: (0, 0),
                event: event::ClaudeStreamEvent::ContentDelta {
                    text: "hello".to_string(),
                },
            },
            None,
        )
        .await;

        let wt = app.worktree_by_id((0, 0)).unwrap();
        assert_eq!(wt.chat_history.len(), 1);
        assert_eq!(wt.chat_history[0].content, "hello");
    }

    #[tokio::test]
    async fn test_claude_complete_event() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.worktree_by_id_mut((0, 0)).unwrap().status = app::WorktreeStatus::Running;

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeComplete {
                worktree_id: (0, 0),
            },
            None,
        )
        .await;

        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().status,
            app::WorktreeStatus::Idle
        );
    }

    #[tokio::test]
    async fn test_claude_error_event() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.worktree_by_id_mut((0, 0)).unwrap().status = app::WorktreeStatus::Running;

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeError {
                worktree_id: (0, 0),
                error: "test error".to_string(),
            },
            None,
        )
        .await;

        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().status,
            app::WorktreeStatus::Idle
        );
        let msg = app.status_message.as_ref().unwrap();
        assert_eq!(msg.level, app::StatusLevel::Error);
        assert!(msg.text.contains("test error"));
    }

    // --- 左パネルキーの状態遷移テスト ---

    #[tokio::test]
    async fn test_left_panel_j_moves_cursor() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(left_panel.cursor_index, 0);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('j'))),
            None,
        )
        .await;
        assert_eq!(left_panel.cursor_index, 1);
    }

    #[tokio::test]
    async fn test_main_panel_i_launches_claude_terminal() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.focused_panel = app::Panel::Main;
        app.selected_worktree = Some((0, 0));
        // active_tab = 0 はチャットタブ
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('i'))),
            None,
        )
        .await;
        // claude がインストールされていない環境ではエラーが表示される
        let launched = claude_terms.contains_key(&((0, 0), 0));
        let errored = app.status_message.is_some();
        assert!(
            launched || errored,
            "Claude 起動またはエラー表示されるべき"
        );
    }

    // --- 右パネルキーの状態遷移テスト ---

    #[tokio::test]
    async fn test_right_panel_t_toggles_mode() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.focused_panel = app::Panel::Right;
        app.selected_worktree = Some((0, 0));
        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().right_panel_mode,
            app::RightPanelMode::Tree
        );

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('t'))),
            None,
        )
        .await;
        assert_eq!(
            app.worktree_by_id((0, 0)).unwrap().right_panel_mode,
            app::RightPanelMode::Diff
        );
    }

    // --- マウスクリックの状態遷移テスト ---

    #[tokio::test]
    async fn test_mouse_click_switches_panel() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // レイアウトを作成（200x50 ターミナル想定）
        let layout = ui::layout::compute_layout(ratatui::prelude::Rect::new(0, 0, 200, 50));

        assert_eq!(app.focused_panel, app::Panel::Left);

        // Main パネルのエリアをクリック
        let mouse = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: layout.main.x + 5,
            row: layout.main.y + 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Main);
    }

    #[tokio::test]
    async fn test_mouse_click_ignored_during_popup() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let layout = ui::layout::compute_layout(ratatui::prelude::Rect::new(0, 0, 200, 50));

        app.show_help = true;
        assert_eq!(app.focused_panel, app::Panel::Left);

        let mouse = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: layout.main.x + 5,
            row: layout.main.y + 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
        )
        .await;
        // ポップアップ表示中はフォーカス変わらない
        assert_eq!(app.focused_panel, app::Panel::Left);
    }

    // --- Worktree 追加ポップアップのテスト ---

    #[tokio::test]
    async fn test_a_key_opens_add_worktree_popup_on_project() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // カーソルはプロジェクト行 (index 0)
        assert!(!app.show_add_worktree_popup);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
        )
        .await;
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.add_worktree_project_index, 0);
        assert!(!app.add_worktree_name.is_empty());
    }

    #[tokio::test]
    async fn test_a_key_opens_add_worktree_popup_on_worktree() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // カーソルを worktree 行に移動
        left_panel.cursor_index = 1;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
        )
        .await;
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.add_worktree_project_index, 0);
    }

    #[tokio::test]
    async fn test_add_worktree_popup_esc_cancels() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        app.add_worktree_input = "feature/test".to_string();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
        )
        .await;
        assert!(!app.show_add_worktree_popup);
        assert!(app.add_worktree_input.is_empty());
    }

    #[tokio::test]
    async fn test_add_worktree_popup_char_input() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        for c in "abc".chars() {
            handle_event(
                &mut app,
                &mut left_panel,
                &mut source_tree,
                &mut diff_view,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
            )
            .await;
        }
        assert_eq!(app.add_worktree_input, "abc");
    }

    #[tokio::test]
    async fn test_add_worktree_popup_backspace() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        app.add_worktree_input = "ab".to_string();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
        )
        .await;
        assert_eq!(app.add_worktree_input, "a");
    }

    #[tokio::test]
    async fn test_add_worktree_popup_enter_empty_branch_does_nothing() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        app.add_worktree_input.clear();
        let worktree_count = app.projects[0].worktrees.len();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
        )
        .await;
        // ポップアップは閉じず、worktree も追加されない
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.projects[0].worktrees.len(), worktree_count);
    }

    #[tokio::test]
    async fn test_add_worktree_popup_blocks_global_keys() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        // ポップアップ中に q を押してもアプリは終了しない
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
        )
        .await;
        assert!(app.running);
        assert_eq!(app.add_worktree_input, "q");
    }

    #[tokio::test]
    async fn test_add_worktree_popup_enter_adds_worktree() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_worktree_popup = true;
        app.add_worktree_project_index = 0;
        app.add_worktree_name = "tokyo".to_string();
        app.add_worktree_input = "feature/auth".to_string();
        let initial_count = app.projects[0].worktrees.len();

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
        )
        .await;

        assert!(!app.show_add_worktree_popup);
        assert_eq!(app.projects[0].worktrees.len(), initial_count + 1);
        let new_wt = app.projects[0].worktrees.last().unwrap();
        assert_eq!(new_wt.name, "tokyo");
        assert_eq!(new_wt.branch, "feature/auth");
        assert_eq!(new_wt.status, app::WorktreeStatus::Idle);
    }

    // --- プロジェクト追加ポップアップのテスト ---

    #[tokio::test]
    async fn test_shift_a_opens_add_project_popup() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(!app.show_add_project_popup);
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('A'))),
            None,
        )
        .await;
        assert!(app.show_add_project_popup);
        // カレントディレクトリが初期値として入る
        assert!(!app.add_project_input.is_empty());
    }

    #[tokio::test]
    async fn test_add_project_popup_esc_cancels() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_project_popup = true;
        app.add_project_input = "/tmp/test".to_string();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
        )
        .await;
        assert!(!app.show_add_project_popup);
        assert!(app.add_project_input.is_empty());
    }

    #[tokio::test]
    async fn test_add_project_popup_char_input() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_project_popup = true;
        for c in "/tmp".chars() {
            handle_event(
                &mut app,
                &mut left_panel,
                &mut source_tree,
                &mut diff_view,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
            )
            .await;
        }
        assert_eq!(app.add_project_input, "/tmp");
    }

    #[tokio::test]
    async fn test_add_project_popup_enter_empty_does_nothing() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_project_popup = true;
        app.add_project_input.clear();
        let project_count = app.projects.len();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
        )
        .await;
        assert!(app.show_add_project_popup);
        assert_eq!(app.projects.len(), project_count);
    }

    #[tokio::test]
    async fn test_add_project_popup_enter_nonexistent_path_shows_error() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_project_popup = true;
        app.add_project_input = "/nonexistent/path/12345".to_string();
        let project_count = app.projects.len();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
        )
        .await;
        assert!(!app.show_add_project_popup);
        assert_eq!(app.projects.len(), project_count);
        assert!(app.status_message.is_some());
        assert_eq!(
            app.status_message.as_ref().unwrap().level,
            app::StatusLevel::Error
        );
    }

    #[tokio::test]
    async fn test_add_project_popup_enter_valid_path_adds_project() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // /tmp は実在するディレクトリ
        app.show_add_project_popup = true;
        app.add_project_input = "/tmp".to_string();
        let initial_count = app.projects.len();
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
        )
        .await;
        assert!(!app.show_add_project_popup);
        assert_eq!(app.projects.len(), initial_count + 1);
        let new_project = app.projects.last().unwrap();
        assert_eq!(new_project.name, "tmp");
        assert!(new_project.worktrees.is_empty());
    }

    #[tokio::test]
    async fn test_add_project_popup_blocks_global_keys() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.show_add_project_popup = true;
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
        )
        .await;
        assert!(app.running);
        assert_eq!(app.add_project_input, "q");
    }

    #[tokio::test]
    async fn test_mouse_click_without_layout() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(app.focused_panel, app::Panel::Left);

        let mouse = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 100,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        // layout が None の場合は何も起きない
        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Left);
    }
}

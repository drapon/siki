mod app;
mod broker;
mod claude;
mod config;
mod db;
mod event;
mod git;
mod hooks;
mod mcp;
mod session;
mod terminal;
mod tui;
mod ui;

use anyhow::Result;
use config::load_config;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ui::diff_view::DiffView;
use ui::left_panel::LeftPanel;
use ui::source_tree::SourceTree;

/// ターミナルエミュレータのキー: (worktree_id, tab_index)
type TerminalKey = (app::WorktreeId, usize);

/// Claude ターミナルを識別する特殊なタブインデックスの基底値
/// 実際のタブインデックス = CLAUDE_TAB_BASE + claude_tab_index
const CLAUDE_TAB_BASE: usize = usize::MAX - 100;

/// siki.json 作成用オーバーレイターミナルのセンチネル値
const SIKI_INIT_WORKTREE_ID: app::WorktreeId = (usize::MAX, usize::MAX);
const SIKI_INIT_TAB_INDEX: usize = usize::MAX;

#[tokio::main]
async fn main() -> Result<()> {
    // サブコマンド: siki mcp → MCP stdio サーバーを起動
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "mcp" {
        let db_path = config::db_path();
        return mcp::run_stdio_server(&db_path);
    }

    tui::install_panic_hook();

    config::ensure_dirs()?;
    let config_path = config::default_config_path();
    let mut config = load_config(&config_path)?;

    // ファイルシステムからプロジェクトを自動検出（config.toml の projects より優先）
    let discovered = config::discover_projects();
    if !discovered.is_empty() {
        config.projects = discovered;
    }

    let shell = config::resolve_shell(&config);
    let mut app = app::App::new(&config);
    let mut left_panel = LeftPanel::new();
    let mut source_tree = SourceTree::new();
    let mut diff_view = DiffView::new();
    let mut sessions: HashMap<app::WorktreeId, claude::ClaudeSession> = HashMap::new();
    let mut terminals: HashMap<TerminalKey, terminal::TerminalEmulator> = HashMap::new();
    let mut claude_terms: HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator> =
        HashMap::new();
    let mut siki_init_terminal: Option<terminal::TerminalEmulator> = None;

    // セッションレジストリ・DB・broker の起動
    let session_registry = Arc::new(Mutex::new(session::SessionRegistry::new()));
    let sock_path = config::sock_path();
    let db_path = config::db_path();
    let broker_db = Arc::new(Mutex::new(db::init(&db_path)?));

    let mut tui_terminal = tui::init()?;
    let mut events = event::EventHandler::new();
    let event_tx = events.sender();
    let mut last_layout: Option<ui::layout::AppLayout> = None;

    // broker タスクを起動（hook イベントを受信）
    match broker::Broker::new(&sock_path, Arc::clone(&session_registry), Arc::clone(&broker_db), event_tx.clone()) {
        Ok(b) => {
            tokio::spawn(b.run());
        }
        Err(e) => {
            eprintln!("broker の起動に失敗（セッション監視は無効）: {}", e);
        }
    }

    // 起動時に全 worktree の PR 情報を非同期取得
    for (pi, project) in app.projects.iter().enumerate() {
        for (wi, wt) in project.worktrees.iter().enumerate() {
            let tx = event_tx.clone();
            let wt_id = (pi, wi);
            let wt_path = wt.path.clone();
            tokio::spawn(async move {
                let title = fetch_pr_title(&wt_path).await;
                let _ = tx.send(event::AppEvent::PrInfo {
                    worktree_id: wt_id,
                    title,
                });
            });
        }
    }

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
        // スクロールバックオフセットを適用してから screen() を取得
        if let Some(wt_id) = app.selected_worktree {
            if let Some(wt) = app.worktree_by_id(wt_id) {
                let tab = wt.active_tab;
                if tab < wt.claude_tabs {
                    let offset = wt.claude_scroll_offset;
                    if let Some(emu) = claude_terms.get_mut(&(wt_id, tab)) {
                        emu.set_scrollback(offset);
                    }
                }
            }
        }
        let claude_screen = app.selected_worktree.and_then(|wt_id| {
            let tab = app.worktree_by_id(wt_id)?.active_tab;
            let claude_tabs = app.worktree_by_id(wt_id)?.claude_tabs;
            if tab < claude_tabs {
                claude_terms.get(&(wt_id, tab)).map(|emu| emu.screen())
            } else {
                None
            }
        });

        // siki.json 作成用オーバーレイターミナルの画面（スクロールバック適用）
        if let Some(emu) = siki_init_terminal.as_mut() {
            emu.set_scrollback(app.siki_json_init_scroll);
        }
        let siki_init_screen = siki_init_terminal.as_ref().map(|emu| emu.screen());

        // UI 描画
        tui_terminal.draw(|frame| {
            let registry = session_registry.lock().unwrap();
            last_layout = Some(ui::render(
                frame,
                &mut app,
                &left_panel,
                &source_tree,
                &diff_view,
                terminal_screen,
                terminal_tab_info.as_ref(),
                claude_screen,
                siki_init_screen,
                Some(&registry),
            ));
        })?;

        // PTY のサイズを描画エリアに合わせてリサイズ
        if let Some(ref layout) = last_layout {
            resize_terminals(
                &app,
                &mut terminals,
                &mut claude_terms,
                &mut siki_init_terminal,
                layout,
            );
        }

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
            &mut siki_init_terminal,
            &event_tx,
            &shell,
            ev,
            last_layout.as_ref(),
            &Some(Arc::clone(&session_registry)),
        )
        .await;
    }

    // クリーンアップ: 全セッションを終了
    for (_id, mut session) in sessions.drain() {
        let _ = session.kill().await;
    }

    // ソケットファイルを削除
    broker::cleanup_socket(&sock_path);

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
    siki_init_terminal: &mut Option<terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    event: event::AppEvent,
    last_layout: Option<&ui::layout::AppLayout>,
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    use event::AppEvent;

    match event {
        AppEvent::Key(key) => {
            // siki.json 作成オーバーレイターミナル表示中
            if app.show_siki_json_init_terminal {
                handle_siki_init_terminal_key(app, siki_init_terminal, key);
                return;
            }

            // ポップアップ表示中はポップアップ専用の処理
            if app.show_help {
                match key.code {
                    KeyCode::Esc => app.show_help = false,
                    KeyCode::Char('j') | KeyCode::Down => app.help_scroll += 1,
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.help_scroll = app.help_scroll.saturating_sub(1);
                    }
                    _ => {}
                }
                return;
            }
            if app.show_session_choice {
                match key.code {
                    KeyCode::Char('1') | KeyCode::Enter | KeyCode::Esc => {
                        // Start new（デフォルト）
                        app.show_session_choice = false;
                        if let Some(wt_id) = app.session_choice_wt_id.take() {
                            launch_claude(app, claude_terms, event_tx, wt_id);
                        }
                    }
                    KeyCode::Char('2') => {
                        // Continue with context:
                        // 1. claude -c -p で前セッションのサマリーを取得
                        // 2. .claude/rules/siki-context.md に書き込む
                        // 3. 新規セッションを起動（サマリーはルールとして読まれる）
                        app.show_session_choice = false;
                        if let Some(wt_id) = app.session_choice_wt_id.take() {
                            let wt_path = app.worktree_by_id(wt_id).unwrap().path.clone();
                            let tx = event_tx.clone();
                            app.show_info("Generating context summary...".to_string());
                            tokio::spawn(async move {
                                let summary = tokio::process::Command::new("claude")
                                    .args(["-c", "-p", "Summarize what you were working on in detail. Include: the goal, what was done, key files changed, decisions made, current status, and any remaining work. Be specific about file paths and code changes."])
                                    .current_dir(&wt_path)
                                    .output()
                                    .await
                                    .ok()
                                    .filter(|o| o.status.success())
                                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                                    .unwrap_or_default();
                                // サマリーを handoff ディレクトリに書き込む
                                let handoff_dir = wt_path.join(".claude/handoff");
                                let _ = std::fs::create_dir_all(&handoff_dir);
                                let context = if summary.is_empty() {
                                    "# Previous Session\n\nNo context available.\n".to_string()
                                } else {
                                    format!("# Previous Session Summary\n\n{}\n", summary)
                                };
                                let _ = std::fs::write(handoff_dir.join("context.md"), &context);
                                let _ = tx.send(event::AppEvent::SessionContext {
                                    worktree_id: wt_id,
                                });
                            });
                        }
                    }
                    _ => {}
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
            if app.show_grep_popup {
                handle_grep_popup_key(app, key);
                return;
            }
            if app.show_archive_confirm {
                handle_archive_confirm_key(app, sessions, terminals, claude_terms, event_tx, shell, key);
                return;
            }
            if app.show_remove_project_confirm {
                handle_remove_project_confirm_key(app, sessions, terminals, claude_terms, key);
                return;
            }
            if app.show_siki_json_confirm {
                handle_siki_json_confirm_key(app, siki_init_terminal, event_tx, key);
                return;
            }

            // Terminal パネルフォーカス中は特別処理
            if app.focused_panel == app::Panel::Terminal {
                handle_terminal_key(app, terminals, event_tx, shell, key);
                return;
            }

            // Main パネルで Claude タブがアクティブな場合はキーを PTY に転送
            if app.focused_panel == app::Panel::Main {
                let on_claude_tab = app
                    .selected_worktree()
                    .map(|wt| wt.active_tab < wt.claude_tabs)
                    .unwrap_or(false);
                if on_claude_tab {
                    handle_claude_terminal_key(app, claude_terms, event_tx, key, session_registry);
                    return;
                }
            }

            // グローバルキー
            match key.code {
                KeyCode::Char('q') => app.running = false,
                KeyCode::Char('?') | KeyCode::F(1) => {
                    app.help_scroll = 0;
                    app.show_help = true;
                }
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
                            // Claude タブは上で早期 return 済みなのでここは非 Claude タブのみ
                            handle_main_panel_key(app, claude_terms, event_tx, key, session_registry);
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
            if worktree_id == SIKI_INIT_WORKTREE_ID && tab_index == SIKI_INIT_TAB_INDEX {
                if let Some(emu) = siki_init_terminal.as_mut() {
                    emu.process(&data);
                }
            } else if tab_index >= CLAUDE_TAB_BASE {
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
            // ヘルプポップアップ表示中はスクロールのみ処理
            if app.show_help {
                match mouse.kind {
                    MouseEventKind::ScrollDown => app.help_scroll += 1,
                    MouseEventKind::ScrollUp => {
                        app.help_scroll = app.help_scroll.saturating_sub(1);
                    }
                    _ => {}
                }
                return;
            }
            // siki.json 作成オーバーレイ表示中はスクロールのみ処理
            if app.show_siki_json_init_terminal {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.siki_json_init_scroll = app.siki_json_init_scroll.saturating_add(3);
                    }
                    MouseEventKind::ScrollDown => {
                        app.siki_json_init_scroll = app.siki_json_init_scroll.saturating_sub(3);
                    }
                    _ => {}
                }
                return;
            }
            // その他のポップアップ表示中はマウスイベント無視
            if app.show_message_popup || app.show_add_worktree_popup || app.show_add_project_popup || app.show_archive_confirm || app.show_remove_project_confirm || app.show_siki_json_confirm {
                return;
            }
            match mouse.kind {
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    if let Some(layout) = last_layout {
                        if let Some(panel) = layout.hit_test(mouse.column, mouse.row) {
                            app.focused_panel = panel;
                        }
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(layout) = last_layout {
                        let panel = layout.hit_test(mouse.column, mouse.row);
                        let is_up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                        match panel {
                            Some(app::Panel::Main) => {
                                let on_claude_tab = app
                                    .selected_worktree()
                                    .map(|wt| wt.active_tab < wt.claude_tabs)
                                    .unwrap_or(false);
                                if on_claude_tab {
                                    if let Some(wt) = app.selected_worktree_mut() {
                                        if is_up {
                                            wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_add(3);
                                        } else {
                                            wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_sub(3);
                                        }
                                    }
                                } else if is_up {
                                    ui::main_panel::scroll_up(app);
                                } else {
                                    ui::main_panel::scroll_down(app);
                                }
                            }
                            Some(app::Panel::Right) => {
                                let mode = app
                                    .selected_worktree()
                                    .map(|wt| wt.right_panel_mode)
                                    .unwrap_or(app::RightPanelMode::Tree);
                                match mode {
                                    app::RightPanelMode::Tree => {
                                        if is_up {
                                            source_tree.move_up();
                                        } else {
                                            source_tree.move_down();
                                        }
                                    }
                                    app::RightPanelMode::Diff => {
                                        if is_up {
                                            diff_view.scroll_up();
                                        } else {
                                            diff_view.scroll_down();
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        AppEvent::PrInfo { worktree_id, title } => {
            if let Some(wt) = app.worktree_by_id_mut(worktree_id) {
                wt.pr_title = title;
            }
        }
        AppEvent::Resize(_w, _h) => {
            // ratatui が自動的にリサイズを処理する
        }
        AppEvent::SessionUpdate { .. } => {
            // セッション状態の変化 — レジストリは broker 側で既に更新済み
        }
        AppEvent::SessionContext { worktree_id } => {
            app.show_info("Launching Claude with previous context...".to_string());
            launch_claude_with_args(app, claude_terms, event_tx, worktree_id, &["--add-dir", ".claude/handoff"]);
        }
        AppEvent::Tick => {
            app.clear_expired_status();
            if app.show_siki_json_init_terminal {
                app.siki_json_init_spinner = app.siki_json_init_spinner.wrapping_add(1);
            }
            // ハートビートタイムアウト: 15秒→Stale、30秒→Dead
            if let Some(registry) = session_registry {
                let mut reg = registry.lock().unwrap();
                reg.expire_stale_sessions(
                    std::time::Duration::from_secs(15),
                    std::time::Duration::from_secs(30),
                );
            }
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
                path: path.clone(),
                worktrees: Vec::new(),
                collapsed: false,
            });

            // project.json を保存（ファイルシステムベースの永続化）
            if let Err(e) = config::save_project_meta(&name, &path) {
                app.show_error(format!("project.json の保存に失敗: {}", e));
            } else {
                app.show_info(format!("プロジェクト追加完了: {}", name));
            }

            app.show_add_project_popup = false;
            app.add_project_input.clear();

            // siki.json が無ければ作成確認ポップアップを表示
            if !config::siki_json_exists(&path) {
                app.siki_json_confirm_project_path = Some(path);
                app.show_siki_json_confirm = true;
            }
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

            // git worktree を作成
            let project_name = app.projects[pi].name.clone();
            let project_path = app.projects[pi].path.clone();
            let wt_path = config::worktree_path(&project_name, &wt_name);

            let shared_dirs = config::load_config(&config::default_config_path())
                .map(|c| c.siki.shared_dirs)
                .unwrap_or_default();

            if let Err(e) = git::WorktreeManager::create_worktree(
                &project_path,
                &wt_path,
                &branch,
                &shared_dirs,
            ) {
                app.show_error(format!("worktree の作成に失敗: {}", e));
                app.show_add_worktree_popup = false;
                app.add_worktree_input.clear();
                return;
            }

            // メモリ上に worktree を追加
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
                claude_scroll_offset: 0,
                pr_title: None,
            });

            // PR 情報を非同期取得
            let wi = app.projects[pi].worktrees.len() - 1;
            let tx = event_tx.clone();
            let pr_path = wt_path.clone();
            tokio::spawn(async move {
                let title = fetch_pr_title(&pr_path).await;
                let _ = tx.send(event::AppEvent::PrInfo {
                    worktree_id: (pi, wi),
                    title,
                });
            });

            app.show_info(format!("worktree 追加完了: {} ({})", wt_name, branch));

            // siki.json の setup スクリプトがあれば実行
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
    let path = app.worktree_by_id(wt_id).unwrap().path.clone();

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
                let project_path = &app.projects[pi].path;

                // siki.json が無ければ作成確認ポップアップを表示
                if !config::siki_json_exists(project_path) {
                    app.siki_json_confirm_project_path = Some(project_path.clone());
                    app.show_siki_json_confirm = true;
                    return;
                }

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
        KeyCode::Char('d') => {
            match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Worktree { project_index, worktree_index }) => {
                    app.archive_target = Some((*project_index, *worktree_index));
                    app.show_archive_confirm = true;
                }
                Some(ui::left_panel::ListEntry::Project { index }) => {
                    app.remove_project_target = Some(*index);
                    app.show_remove_project_confirm = true;
                }
                None => {}
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
                // worktree のパスからソースツリーと diff を読み込む
                let wt_path = app.worktree_by_id(wt_id).unwrap().path.clone();
                source_tree.load(&wt_path);
                diff_view.load(&wt_path);
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
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
) {
    use crossterm::event::KeyCode;

    // ファイル内検索モード中
    if ui::main_panel::is_file_search_active(app) {
        match key.code {
            KeyCode::Esc => ui::main_panel::file_search_cancel(app),
            KeyCode::Enter => ui::main_panel::file_search_confirm(app),
            KeyCode::Backspace => ui::main_panel::file_search_pop(app),
            KeyCode::Char(c) => ui::main_panel::file_search_push(app, c),
            _ => {}
        }
        return;
    }

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
        KeyCode::Char('/') => {
            ui::main_panel::file_search_start(app);
        }
        KeyCode::Char('n') => {
            ui::main_panel::file_next_match(app);
        }
        KeyCode::Char('N') => {
            ui::main_panel::file_prev_match(app);
        }
        KeyCode::Char('g') => {
            if app.selected_worktree.is_some() {
                app.show_grep_popup = true;
                app.grep_input.clear();
                app.grep_results.clear();
                app.grep_cursor = 0;
            }
        }
        KeyCode::Char('i') => {
            // 新しい Claude Code タブを追加
            if let Some(wt_id) = app.selected_worktree {
                // 同じ worktree にアクティブセッションがあるかチェック
                if has_active_sessions(session_registry, app, wt_id) {
                    app.show_session_choice = true;
                    app.session_choice_wt_id = Some(wt_id);
                } else {
                    launch_claude(app, claude_terms, event_tx, wt_id);
                }
            }
        }
        KeyCode::Char('s') => {
            // ファイルパス:行番号を最初の Claude Code PTY に送信
            if let Some(location) = ui::main_panel::current_file_location(app) {
                if let Some(wt_id) = app.selected_worktree {
                    if let Some(emu) = claude_terms.get_mut(&(wt_id, 0)) {
                        let msg = format!("{}\n", location);
                        if let Err(e) = emu.write(msg.as_bytes()) {
                            app.show_error(format!("Claude への送信に失敗: {}", e));
                        } else {
                            app.show_info(format!("送信: {}", location));
                        }
                    } else {
                        app.show_error("Claude Code が起動していません".to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// siki.json 作成確認ダイアログのキー処理
fn handle_siki_json_confirm_key(
    app: &mut app::App,
    siki_init_terminal: &mut Option<terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(path) = app.siki_json_confirm_project_path.take() {
                let prompt = config::siki_json_init_prompt();
                match terminal::TerminalEmulator::with_args(
                    "claude",
                    &[&prompt],
                    &path,
                    (80, 24),
                    event_tx.clone(),
                    SIKI_INIT_WORKTREE_ID,
                    SIKI_INIT_TAB_INDEX,
                ) {
                    Ok(emu) => {
                        *siki_init_terminal = Some(emu);
                        app.show_siki_json_init_terminal = true;
                        app.siki_json_init_scroll = 0;
                        app.show_info("Claude を起動中...".to_string());
                    }
                    Err(e) => app.show_error(format!("Claude 起動失敗: {}", e)),
                }
            }
            app.show_siki_json_confirm = false;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.siki_json_confirm_project_path = None;
            app.show_siki_json_confirm = false;
        }
        _ => {}
    }
}

/// siki.json 作成オーバーレイターミナルのキー処理
fn handle_siki_init_terminal_key(
    app: &mut app::App,
    siki_init_terminal: &mut Option<terminal::TerminalEmulator>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Esc または Ctrl+\ でオーバーレイを閉じる
    if key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('\\') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        *siki_init_terminal = None;
        app.show_siki_json_init_terminal = false;
        app.siki_json_init_scroll = 0;
        return;
    }

    // Shift+PageUp/PageDown でスクロールバック操作
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::PageUp => {
                app.siki_json_init_scroll = app.siki_json_init_scroll.saturating_add(10);
                return;
            }
            KeyCode::PageDown => {
                app.siki_json_init_scroll = app.siki_json_init_scroll.saturating_sub(10);
                return;
            }
            _ => {}
        }
    }

    // 入力操作時はスクロールを最新に戻す
    app.siki_json_init_scroll = 0;

    // その他のキーは PTY に転送
    let bytes = terminal::key_to_bytes(&key);
    if !bytes.is_empty() {
        if let Some(emu) = siki_init_terminal.as_mut() {
            let _ = emu.write(&bytes);
        }
    }
}

/// Grep ポップアップのキー処理
fn handle_grep_popup_key(app: &mut app::App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.show_grep_popup = false;
        }
        KeyCode::Enter => {
            if app.grep_results.is_empty() {
                // 結果がない → 検索実行
                app.run_grep();
            } else {
                // 結果あり → 選択中のファイルを開く
                if let Some(result) = app.grep_results.get(app.grep_cursor).cloned() {
                    app.show_grep_popup = false;
                    ui::main_panel::open_file_tab(app, result.path);
                    // 該当行にカーソルを移動
                    if let Some(wt) = app.selected_worktree_mut() {
                        let file_index = wt.active_tab.saturating_sub(wt.claude_tabs);
                        if let Some(file) = wt.open_files.get_mut(file_index) {
                            let target = result.line_number.saturating_sub(1); // 1-indexed → 0-indexed
                            if target < file.highlighted.len() {
                                file.cursor_line = target;
                            }
                        }
                    }
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !app.grep_results.is_empty() {
                app.grep_cursor = (app.grep_cursor + 1).min(app.grep_results.len() - 1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.grep_cursor = app.grep_cursor.saturating_sub(1);
        }
        KeyCode::Backspace => {
            app.grep_input.pop();
            // 入力変更時は結果をリセット
            app.grep_results.clear();
            app.grep_cursor = 0;
        }
        KeyCode::Char(c) => {
            app.grep_input.push(c);
            // 入力変更時は結果をリセット
            app.grep_results.clear();
            app.grep_cursor = 0;
        }
        _ => {}
    }
}

/// アーカイブ確認ダイアログのキー処理
/// プロジェクト除外確認ダイアログのキー処理
fn handle_remove_project_confirm_key(
    app: &mut app::App,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Some(pi) = app.remove_project_target {
                let project_name = app.projects[pi].name.clone();

                // 関連する Claude セッション・ターミナルをクリーンアップ
                let wt_count = app.projects[pi].worktrees.len();
                for wi in 0..wt_count {
                    let wt_id = (pi, wi);
                    sessions.remove(&wt_id);
                    let term_keys: Vec<_> = terminals
                        .keys()
                        .filter(|k| k.0 == wt_id)
                        .cloned()
                        .collect();
                    for key in term_keys {
                        terminals.remove(&key);
                    }
                    let claude_keys: Vec<_> = claude_terms
                        .keys()
                        .filter(|k| k.0 == wt_id)
                        .cloned()
                        .collect();
                    for key in claude_keys {
                        claude_terms.remove(&key);
                    }
                }

                // project.json を削除
                if let Err(e) = config::remove_project_meta(&project_name) {
                    app.show_error(format!("プロジェクトメタの削除に失敗: {}", e));
                }

                // メモリから削除
                app.projects.remove(pi);

                // selected_worktree をリセット
                if let Some((sel_pi, sel_wi)) = app.selected_worktree {
                    if sel_pi == pi {
                        app.selected_worktree = None;
                    } else if sel_pi > pi {
                        app.selected_worktree = Some((sel_pi - 1, sel_wi));
                    }
                }

                app.show_info(format!("プロジェクトを除外しました: {}", project_name));
            }

            app.show_remove_project_confirm = false;
            app.remove_project_target = None;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.show_remove_project_confirm = false;
            app.remove_project_target = None;
        }
        _ => {}
    }
}

/// アーカイブ確認ダイアログのキー処理
fn handle_archive_confirm_key(
    app: &mut app::App,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Some((pi, wi)) = app.archive_target {
                let project_path = app.projects[pi].path.clone();
                let wt_name = app.projects[pi].worktrees[wi].name.clone();
                let wt_path = app.projects[pi].worktrees[wi].path.clone();
                let wt_id = (pi, wi);

                // siki.json の archive スクリプトがあれば実行
                if let Some(siki_json) = config::load_siki_json(&project_path) {
                    if let Some(ref archive_script) = siki_json.scripts.archive {
                        run_siki_script(
                            app, terminals, event_tx, shell,
                            wt_id, archive_script, &wt_name, &wt_path,
                        );
                    }
                }

                // 関連する Claude セッションをクリーンアップ
                sessions.remove(&wt_id);

                // 関連するターミナルをクリーンアップ
                let term_keys: Vec<_> = terminals
                    .keys()
                    .filter(|k| k.0 == wt_id)
                    .cloned()
                    .collect();
                for key in term_keys {
                    terminals.remove(&key);
                }

                // 関連する Claude ターミナルをクリーンアップ
                let claude_keys: Vec<_> = claude_terms
                    .keys()
                    .filter(|k| k.0 == wt_id)
                    .cloned()
                    .collect();
                for key in claude_keys {
                    claude_terms.remove(&key);
                }

                // git worktree を削除
                match git::WorktreeManager::remove_worktree(&project_path, &wt_path) {
                    Ok(()) => {
                        // メモリから worktree を削除
                        app.projects[pi].worktrees.remove(wi);

                        // selected_worktree をリセット（削除対象 or インデックスずれ対応）
                        if let Some((sel_pi, sel_wi)) = app.selected_worktree {
                            if sel_pi == pi && sel_wi == wi {
                                app.selected_worktree = None;
                            } else if sel_pi == pi && sel_wi > wi {
                                app.selected_worktree = Some((sel_pi, sel_wi - 1));
                            }
                        }

                        app.show_info(format!("worktree を削除しました: {}", wt_name));
                    }
                    Err(e) => {
                        app.show_error(format!("worktree の削除に失敗: {}", e));
                    }
                }
            }

            app.show_archive_confirm = false;
            app.archive_target = None;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.show_archive_confirm = false;
            app.archive_target = None;
        }
        _ => {}
    }
}

/// PTY ターミナルを実際の描画エリアに合わせてリサイズする
fn resize_terminals(
    app: &app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    siki_init_terminal: &mut Option<terminal::TerminalEmulator>,
    layout: &ui::layout::AppLayout,
) {
    // Claude ターミナル: main パネルからタブバー(2行) + ボーダー(2行2列) を引いた内部サイズ
    let claude_cols = layout.main.width.saturating_sub(2);
    let claude_rows = layout.main.height.saturating_sub(4); // tab bar 2 + border 2

    // 右下ターミナル: ボーダー(2行2列) を引いた内部サイズ
    let term_cols = layout.right_bottom.width.saturating_sub(2);
    let term_rows = layout.right_bottom.height.saturating_sub(2);

    if claude_cols > 0 && claude_rows > 0 {
        for emu in claude_terms.values_mut() {
            let screen = emu.screen();
            let (cur_rows, cur_cols) = screen.size();
            if cur_cols != claude_cols || cur_rows != claude_rows {
                let _ = emu.resize(claude_cols, claude_rows);
            }
        }
    }

    if term_cols > 0 && term_rows > 0 {
        if let Some(wt_id) = app.selected_worktree {
            for (key, emu) in terminals.iter_mut() {
                if key.0 == wt_id {
                    let screen = emu.screen();
                    let (cur_rows, cur_cols) = screen.size();
                    if cur_cols != term_cols || cur_rows != term_rows {
                        let _ = emu.resize(term_cols, term_rows);
                    }
                }
            }
        }
    }

    // siki.json 作成オーバーレイターミナル: 全画面の90%x80%からボーダー分を引く
    if let Some(emu) = siki_init_terminal.as_mut() {
        // フレーム全体サイズをレイアウトから復元
        let full_width = layout.left.width + layout.main.width + layout.right_top.width;
        let full_height = layout.left.height + layout.status_bar.height;
        // centered_rect(90, 80) と同じ割合
        let overlay_cols = full_width.saturating_mul(90).saturating_div(100).saturating_sub(2);
        let overlay_rows = full_height.saturating_mul(80).saturating_div(100).saturating_sub(2);
        if overlay_cols > 0 && overlay_rows > 0 {
            let screen = emu.screen();
            let (cur_rows, cur_cols) = screen.size();
            if cur_cols != overlay_cols || cur_rows != overlay_rows {
                let _ = emu.resize(overlay_cols, overlay_rows);
            }
        }
    }
}

/// Claude Code を中央パネルで起動する
///
/// プロジェクトのパスで `claude` をインタラクティブに起動する。
/// 同じ worktree 内にアクティブセッションがあるか判定する
fn has_active_sessions(
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
    app: &app::App,
    wt_id: app::WorktreeId,
) -> bool {
    let Some(reg) = session_registry.as_ref().and_then(|r| r.lock().ok()) else {
        return false;
    };
    let Some(wt) = app.worktree_by_id(wt_id) else {
        return false;
    };
    let project = &app.projects[wt_id.0].name;
    reg.by_worktree(project, &wt.name)
        .iter()
        .any(|s| !matches!(s.state, session::SessionState::Dead | session::SessionState::Stale))
}

fn launch_claude(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    wt_id: app::WorktreeId,
) {
    launch_claude_with_args(app, claude_terms, event_tx, wt_id, &[]);
}

fn launch_claude_with_args(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    wt_id: app::WorktreeId,
    args: &[&str],
) {
    let project_path = app.worktree_by_id(wt_id).unwrap().path.clone();

    // worktree に siki 用 hook を注入
    let sock = config::sock_path();
    if let Err(e) = hooks::ensure_hooks_configured(&project_path, &sock) {
        app.show_error(format!("hook 注入に失敗: {}", e));
    }
    let claude_idx = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.claude_tabs)
        .unwrap_or(0);
    let size = (80, 24);

    let result = if args.is_empty() {
        terminal::TerminalEmulator::new(
            "claude",
            &project_path,
            size,
            event_tx.clone(),
            wt_id,
            CLAUDE_TAB_BASE + claude_idx,
        )
    } else {
        terminal::TerminalEmulator::with_args(
            "claude",
            args,
            &project_path,
            size,
            event_tx.clone(),
            wt_id,
            CLAUDE_TAB_BASE + claude_idx,
        )
    };

    match result {
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
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    key: crossterm::event::KeyEvent,
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let Some(wt_id) = app.selected_worktree else {
        return;
    };

    let active_tab = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.active_tab)
        .unwrap_or(0);

    // Ctrl+w で Claude ターミナルを閉じる
    if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
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

    // Ctrl+t で新しい Claude タブを追加（アクティブセッションチェック付き）
    if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if has_active_sessions(session_registry, app, wt_id) {
            app.show_session_choice = true;
            app.session_choice_wt_id = Some(wt_id);
        } else {
            launch_claude(app, claude_terms, event_tx, wt_id);
        }
        return;
    }

    // Ctrl+r で claude -r（セッション再開）タブを追加
    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
        launch_claude_with_args(app, claude_terms, event_tx, wt_id, &["-r"]);
        return;
    }

    // Tab でタブ切り替え
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        ui::main_panel::next_tab(app);
        return;
    }

    // Shift+Up/Down/PageUp/PageDown でスクロールバック操作
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up => {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_add(1);
                }
                return;
            }
            KeyCode::Down => {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_sub(1);
                }
                return;
            }
            KeyCode::PageUp => {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_add(10);
                }
                return;
            }
            KeyCode::PageDown => {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.claude_scroll_offset = wt.claude_scroll_offset.saturating_sub(10);
                }
                return;
            }
            _ => {}
        }
    }

    // 入力操作時はスクロールを最新に戻す
    if let Some(wt) = app.worktree_by_id_mut(wt_id) {
        wt.claude_scroll_offset = 0;
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

/// worktree のパスで `gh pr view` を実行し、PR タイトルを取得する
async fn fetch_pr_title(wt_path: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args(["pr", "view", "--json", "title", "--jq", ".title"])
        .current_dir(wt_path)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
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

    fn sample_config_with_path(path: String) -> Config {
        Config {
            siki: SikiConfig {
                shell: Some("/bin/sh".to_string()),
                shared_dirs: vec![],
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('?'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::BackTab)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(shift_key(KeyCode::Tab)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(ctrl_key('\\')),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
            &None,
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
        let mut siki_init_terminal = None;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeOutput {
                worktree_id: (0, 0),
                event: event::ClaudeStreamEvent::ContentDelta {
                    text: "hello".to_string(),
                },
            },
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeComplete {
                worktree_id: (0, 0),
            },
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::ClaudeError {
                worktree_id: (0, 0),
                error: "test error".to_string(),
            },
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('j'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('i'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('t'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
            &None,
        )
        .await;
        // ポップアップ表示中はフォーカス変わらない
        assert_eq!(app.focused_panel, app::Panel::Left);
    }

    // --- Worktree 追加ポップアップのテスト ---

    #[tokio::test]
    async fn test_a_key_opens_add_worktree_popup_on_project() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("siki.json"), r#"{"scripts":{}}"#).unwrap();
        let config = sample_config_with_path(dir.path().to_string_lossy().to_string());
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
        )
        .await;
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.add_worktree_project_index, 0);
        assert!(!app.add_worktree_name.is_empty());
    }

    #[tokio::test]
    async fn test_a_key_opens_add_worktree_popup_on_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("siki.json"), r#"{"scripts":{}}"#).unwrap();
        let config = sample_config_with_path(dir.path().to_string_lossy().to_string());
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
        )
        .await;
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.add_worktree_project_index, 0);
    }

    #[tokio::test]
    async fn test_a_key_shows_siki_json_confirm_when_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        // siki.json を作成しない
        let config = sample_config_with_path(dir.path().to_string_lossy().to_string());
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let mut siki_init_terminal = None;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        handle_event(
            &mut app,
            &mut left_panel,
            &mut source_tree,
            &mut diff_view,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
        )
        .await;
        assert!(app.show_siki_json_confirm);
        assert!(!app.show_add_worktree_popup);
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
        )
        .await;
        assert!(app.running);
        assert_eq!(app.add_worktree_input, "q");
    }

    #[tokio::test]
    async fn test_add_worktree_popup_enter_adds_worktree() {
        // git worktree add を実行するため、実際の git リポジトリが必要
        let temp_dir = tempfile::TempDir::new().unwrap();
        let project_path = temp_dir.path();

        std::process::Command::new("git").args(["init"]).current_dir(project_path).output().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(project_path).output().unwrap();
        std::process::Command::new("git").args(["config", "user.name", "Test"]).current_dir(project_path).output().unwrap();
        std::fs::write(project_path.join("README.md"), "# Test").unwrap();
        std::process::Command::new("git").args(["add", "."]).current_dir(project_path).output().unwrap();
        std::process::Command::new("git").args(["commit", "-m", "initial"]).current_dir(project_path).output().unwrap();

        let config = Config {
            siki: SikiConfig {
                shell: Some("/bin/sh".to_string()),
                shared_dirs: vec![],
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path: project_path.to_string_lossy().to_string(),
                worktrees: vec![WorktreeConfig {
                    name: "feature".to_string(),
                    branch: "feature/test".to_string(),
                }],
            }],
        };
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut sessions = HashMap::new();
        let mut terminals = HashMap::new();
        let mut claude_terms = HashMap::new();
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
        )
        .await;

        assert!(!app.show_add_worktree_popup);
        assert_eq!(app.projects[0].worktrees.len(), initial_count + 1);
        let new_wt = app.projects[0].worktrees.last().unwrap();
        assert_eq!(new_wt.name, "tokyo");
        assert_eq!(new_wt.branch, "feature/auth");
        assert_eq!(new_wt.status, app::WorktreeStatus::Idle);

        // テスト後に作成された worktree をクリーンアップ
        let wt_path = config::worktree_path("test-project", "tokyo");
        let _ = git::WorktreeManager::remove_worktree(project_path, &wt_path);
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('A'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
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
        let mut siki_init_terminal = None;
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
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            None,
            &None,
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Left);
    }
}

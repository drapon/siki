mod app;
mod broker;
mod claude;
mod config;
mod db;
mod event;
mod git;
mod hooks;
mod mcp;
mod selection;
mod session;
mod terminal;
mod tui;
mod ui;

use anyhow::Result;
use config::load_config;
use std::collections::HashMap;
use std::io::Write as _;
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
    let args: Vec<String> = std::env::args().collect();

    // --version / -V
    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V" || args[1] == "-v") {
        println!("siki {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // --help / -h
    if args.len() > 1 && (args[1] == "--help" || args[1] == "-h") {
        println!("siki {} - TUI orchestrator for multiple Claude Code sessions", env!("CARGO_PKG_VERSION"));
        println!();
        println!("Usage: siki [OPTIONS] [COMMAND]");
        println!();
        println!("Commands:");
        println!("  list       List projects and worktrees");
        println!("  mcp        Start MCP stdio server");
        println!();
        println!("Options:");
        println!("  -p <name>  Filter projects by name prefix");
        println!("  -h, --help     Print help");
        println!("  -v, -V, --version  Print version");
        return Ok(());
    }

    // サブコマンド: siki list → プロジェクト一覧を表示
    if args.len() > 1 && args[1] == "list" {
        let projects = config::discover_projects();
        if projects.is_empty() {
            println!("No projects found.");
        } else {
            for p in &projects {
                println!("{} ({})", p.name, p.path);
                for w in &p.worktrees {
                    println!("  └ {} [{}]", w.name, w.branch);
                }
            }
        }
        return Ok(());
    }

    // サブコマンド: siki mcp → MCP stdio サーバーを起動
    if args.len() > 1 && args[1] == "mcp" {
        let db_path = config::db_path();
        return mcp::run_stdio_server(&db_path);
    }

    // -p <prefix> オプションの取得
    let prefix_filter: Option<String> = {
        let mut filter = None;
        let mut i = 1;
        while i < args.len() {
            if args[i] == "-p" {
                if let Some(val) = args.get(i + 1) {
                    filter = Some(val.clone());
                } else {
                    eprintln!("Error: -p requires a project name prefix");
                    std::process::exit(1);
                }
                break;
            }
            i += 1;
        }
        filter
    };

    tui::install_panic_hook();

    config::ensure_dirs()?;
    let config_path = config::default_config_path();
    let mut config = load_config(&config_path)?;

    // ファイルシステムからプロジェクトを自動検出（config.toml の projects より優先）
    let discovered = config::discover_projects();
    if !discovered.is_empty() {
        config.projects = discovered;
    }

    // プロジェクトフィルタの適用
    if let Some(ref prefix) = prefix_filter {
        config.projects = config::filter_projects_by_prefix(config.projects, prefix);
        if config.projects.is_empty() {
            eprintln!("No projects matching prefix: {}", prefix);
            std::process::exit(1);
        }
    } else if let Ok(cwd) = std::env::current_dir() {
        if let Some(project_name) = config::detect_project_from_cwd(&config.projects, &cwd) {
            config.projects.retain(|p| p.name == project_name);
        }
    }

    let shell = config::resolve_shell(&config);
    let mut app = app::App::new(&config);
    let mut left_panel = LeftPanel::new();
    let mut source_tree = SourceTree::new();
    let mut diff_view = DiffView::new();
    let mut local_changes = ui::diff_view::LocalChangesView::new();
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

    // DB から前回の claude_session_id を各 worktree に復元
    if let Ok(conn) = broker_db.lock() {
        for project in &mut app.projects {
            for wt in &mut project.worktrees {
                if let Ok(Some(id)) =
                    db::get_latest_claude_session_id(&conn, &wt.name, &project.name)
                {
                    wt.claude_session_id = Some(id);
                }
            }
        }
    }

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

            let tabs: Vec<usize> = (0..ui::MAX_TERMINAL_TABS)
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
        // NOTE: alternate screen ではスクロールバックが効かないため、
        // set_scrollback は行わず、マウススクロールは PTY に転送する方式に変更済み
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

        // grep 結果の表示行を構築
        let grep_rows = if app.show_grep_results_view {
            let wt_path = app.selected_worktree().map(|wt| wt.path.clone());
            if let Some(wt_path) = wt_path {
                ui::grep_view::build_display_rows(&app.grep_results, &wt_path)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // UI 描画
        tui_terminal.draw(|frame| {
            let registry = session_registry.lock().unwrap();
            last_layout = Some(ui::render(
                frame,
                &mut app,
                &mut left_panel,
                &mut source_tree,
                &diff_view,
                &local_changes,
                terminal_screen,
                terminal_tab_info.as_ref(),
                claude_screen,
                siki_init_screen,
                Some(&registry),
                &grep_rows,
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &event_tx,
            &shell,
            ev,
            last_layout.as_ref(),
            &Some(Arc::clone(&session_registry)),
            &broker_db,
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
    local_changes: &mut ui::diff_view::LocalChangesView,
    sessions: &mut HashMap<app::WorktreeId, claude::ClaudeSession>,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    siki_init_terminal: &mut Option<terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    event: event::AppEvent,
    last_layout: Option<&ui::layout::AppLayout>,
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
    broker_db: &Arc<Mutex<rusqlite::Connection>>,
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

            // スキル内容入力ポップアップ表示中
            if app.show_skill_edit_popup {
                handle_skill_edit_popup_key(app, key, event_tx);
                return;
            }

            // シンボリックリンク設定ポップアップ表示中
            if app.show_symlink_settings {
                handle_symlink_settings_key(app, key);
                return;
            }

            // スキル一覧ポップアップ表示中
            if app.show_skill_list {
                handle_skill_list_key(app, key);
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
                        // Resume session - インタラクティブ選択で過去セッションから選ぶ
                        app.show_session_choice = false;
                        if let Some(wt_id) = app.session_choice_wt_id.take() {
                            launch_claude_with_args(app, claude_terms, event_tx, wt_id, &["-r"]);
                        }
                    }
                    KeyCode::Char('3') => {
                        // Continue with context - 現セッションのサマリーを生成して新セッションに引き継ぐ
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
                                let _ = tx.send(event::AppEvent::SessionContext {
                                    worktree_id: wt_id,
                                    summary,
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
            if app.show_rename_project_popup {
                handle_rename_project_popup_key(app, key);
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
            if app.show_skill_name_popup {
                handle_skill_name_popup_key(app, key);
                return;
            }

            // 選択中の y / Ctrl+C でコピー、Esc で選択解除
            if app.text_selection.is_some() {
                let is_copy = key.code == KeyCode::Char('y')
                    || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL));
                if is_copy {
                    copy_selection(app, claude_terms, terminals);
                    app.text_selection = None;
                    return;
                } else if key.code == KeyCode::Esc {
                    app.text_selection = None;
                    return;
                } else {
                    // その他のキーで選択解除して通常処理へ
                    app.text_selection = None;
                }
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
                            handle_left_panel_key(app, left_panel, source_tree, diff_view, local_changes, terminals, claude_terms, event_tx, shell, key);
                        }
                        app::Panel::Main => {
                            // Claude タブは上で早期 return 済みなのでここは非 Claude タブのみ
                            handle_main_panel_key(app, claude_terms, event_tx, key, session_registry);
                        }
                        app::Panel::Right => {
                            handle_right_panel_key(app, source_tree, diff_view, local_changes, key);
                        }
                        app::Panel::Terminal => unreachable!(),
                    }
                }
            }
        }
        AppEvent::ClaudeOutput {
            worktree_id,
            event: ref stream_event,
        } => {
            // Init イベントの claude_session_id を DB に保存
            if let event::ClaudeStreamEvent::Init { session_id } = stream_event {
                if !session_id.is_empty() {
                    if let Some(wt) = app.worktree_by_id(worktree_id) {
                        let wt_name = wt.name.clone();
                        let project_name = app
                            .projects
                            .iter()
                            .find(|p| {
                                p.worktrees.iter().any(|w| w.name == wt_name)
                            })
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                        if let Ok(conn) = broker_db.lock() {
                            // worktree に紐づくアクティブなセッションの claude_session_id を更新
                            let _ = conn.execute(
                                "UPDATE sessions SET claude_session_id = ?1
                                 WHERE worktree_name = ?2 AND project_name = ?3 AND state != 'dead'",
                                rusqlite::params![session_id, wt_name, project_name],
                            );
                        }
                    }
                }
            }
            app.handle_claude_output(worktree_id, stream_event.clone());
        }
        AppEvent::ClaudeComplete { worktree_id } => {
            app.handle_claude_complete(worktree_id);
            // 選択中の worktree が完了した場合、Diff と SourceTree を自動更新
            if app.selected_worktree == Some(worktree_id) {
                if let Some(wt) = app.worktree_by_id(worktree_id) {
                    let wt_path = wt.path.clone();
                    let base = resolve_base_branch(&app.projects[worktree_id.0].path, &app.projects[worktree_id.0].name);
                    diff_view.load(&wt_path, &base);
                    local_changes.load(&wt_path);
                    source_tree.load(&wt_path);
                }
            }
            // Claude セッション中に PR が作成された可能性があるため再取得
            if let Some(wt) = app.worktree_by_id(worktree_id) {
                let tx = event_tx.clone();
                let wt_path = wt.path.clone();
                let wt_id = worktree_id;
                tokio::spawn(async move {
                    let title = fetch_pr_title(&wt_path).await;
                    let _ = tx.send(event::AppEvent::PrInfo {
                        worktree_id: wt_id,
                        title,
                    });
                });
            }
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
                    // vt100 はスクロールバック中に新行が来ると内部で offset を
                    // 調整するため、外部保持のオフセットを同期する
                    let current = emu.scrollback();
                    if current > 0 {
                        if let Some(wt) = app.worktree_by_id_mut(worktree_id) {
                            wt.claude_scroll_offsets.insert(claude_idx, current);
                        }
                    }
                }
            } else if let Some(emu) = terminals.get_mut(&(worktree_id, tab_index)) {
                emu.process(&data);
            }
        }
        AppEvent::TerminalExited {
            worktree_id,
            tab_index,
        } => {
            if worktree_id == SIKI_INIT_WORKTREE_ID && tab_index == SIKI_INIT_TAB_INDEX {
                if let Some(emu) = siki_init_terminal.as_mut() {
                    emu.mark_exited();
                }
            } else if tab_index >= CLAUDE_TAB_BASE {
                let claude_idx = tab_index - CLAUDE_TAB_BASE;
                if let Some(emu) = claude_terms.get_mut(&(worktree_id, claude_idx)) {
                    emu.mark_exited();
                }
            } else if let Some(emu) = terminals.get_mut(&(worktree_id, tab_index)) {
                emu.mark_exited();
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
            if app.show_message_popup || app.show_add_worktree_popup || app.show_add_project_popup || app.show_rename_project_popup || app.show_archive_confirm || app.show_remove_project_confirm || app.show_siki_json_confirm || app.show_skill_name_popup || app.show_skill_edit_popup || app.show_skill_list || app.show_symlink_settings {
                return;
            }
            match mouse.kind {
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    if let Some(layout) = last_layout {
                        if let Some(panel) = layout.hit_test(mouse.column, mouse.row) {
                            app.focused_panel = panel;
                        }
                        let hit_panel = layout.hit_test(mouse.column, mouse.row);
                        // ターミナルタブのクリック → タブ切替
                        if hit_panel == Some(app::Panel::Terminal)
                            && mouse.row == layout.right_bottom.y
                        {
                            handle_terminal_tab_click(
                                app, terminals, layout.right_bottom, mouse.column,
                            );
                        }
                        // 中央ペインのタブバークリック → タブ切替
                        if hit_panel == Some(app::Panel::Main)
                            && mouse.row == layout.main.y + 1
                        {
                            let clicked_tab = ui::main_panel::tab_index_at(
                                app, layout.main.x, mouse.column,
                            );
                            if let Some(idx) = clicked_tab {
                                let has_search = app.show_grep_results_view;
                                if let Some(wt) = app.selected_worktree_mut() {
                                    let search_count = if has_search { 1 } else { 0 };
                                    let tab_count = wt.claude_tabs + search_count + wt.open_files.len();
                                    if idx < tab_count {
                                        wt.active_tab = idx;
                                    }
                                }
                            }
                        }
                        // Search タブでのクリック → カーソル移動
                        if app.show_grep_results_view
                            && hit_panel == Some(app::Panel::Main)
                        {
                            // main 領域の上部: ヘッダー(1) + タブバー(2) + ボーダー(1) + 検索入力(1) = 5行分オフセット
                            let content_top = layout.main.y + 5;
                            if mouse.row >= content_top {
                                let clicked_row = (mouse.row - content_top) as usize;
                                let target = app.grep_view_scroll + clicked_row;
                                let wt_path = app.selected_worktree().map(|wt| wt.path.clone());
                                let row_count = if let Some(ref wt_path) = wt_path {
                                    ui::grep_view::build_display_rows(&app.grep_results, wt_path).len()
                                } else {
                                    0
                                };
                                if target < row_count {
                                    app.grep_view_cursor = target;
                                }
                            }
                        }
                        // ファイルタブのコンテンツ領域でクリック → カーソル移動 + 選択開始
                        let mut file_selection_started = false;
                        if hit_panel == Some(app::Panel::Main)
                            && mouse.row > layout.main.y + 1
                            && app.file_content_area.is_some()
                        {
                            let on_file_tab = app
                                .selected_worktree()
                                .map(|wt| {
                                    let search_offset = if app.show_grep_results_view { 1 } else { 0 };
                                    wt.active_tab >= wt.claude_tabs + search_offset
                                })
                                .unwrap_or(false);
                            if on_file_tab {
                                ui::main_panel::click_file_line(app, layout.main, mouse.row);
                                let content_area = app.file_content_area.unwrap();
                                let pos = selection::screen_to_term(mouse.column, mouse.row, &content_area);
                                let file_scroll = app.selected_worktree()
                                    .and_then(|wt| {
                                        let search_offset = if app.show_grep_results_view { 1 } else { 0 };
                                        let fi = wt.active_tab.checked_sub(wt.claude_tabs + search_offset)?;
                                        wt.open_files.get(fi).map(|f| f.scroll_offset)
                                    })
                                    .unwrap_or(0);
                                app.text_selection = Some(selection::TextSelection {
                                    anchor: pos,
                                    current: pos,
                                    active: true,
                                    panel: selection::SelectionPanel::File,
                                    scroll_at_select: file_scroll,
                                });
                                file_selection_started = true;
                            }
                        }
                        // Claude ペインのコンテンツ領域でクリック → 選択開始
                        let on_claude_tab = !file_selection_started && app
                            .selected_worktree()
                            .map(|wt| wt.active_tab < wt.claude_tabs)
                            .unwrap_or(false);
                        if on_claude_tab
                            && hit_panel == Some(app::Panel::Main)
                            && app.claude_content_area.is_some()
                        {
                            let content_area = app.claude_content_area.unwrap();
                            let pos = selection::screen_to_term(mouse.column, mouse.row, &content_area);
                            let wt_id = app.selected_worktree.unwrap();
                            let tab = app.selected_worktree().map(|wt| wt.active_tab).unwrap_or(0);
                            let scrollback = claude_terms.get(&(wt_id, tab)).map(|emu| emu.scrollback()).unwrap_or(0);
                            app.text_selection = Some(selection::TextSelection {
                                anchor: pos,
                                current: pos,
                                active: true,
                                panel: selection::SelectionPanel::Claude,
                                scroll_at_select: scrollback,
                            });
                        } else if hit_panel == Some(app::Panel::Terminal)
                            && app.terminal_content_area.is_some()
                        {
                            let content_area = app.terminal_content_area.unwrap();
                            let pos = selection::screen_to_term(mouse.column, mouse.row, &content_area);
                            let scrollback = app.selected_worktree.and_then(|wt_id| {
                                let tab = app.worktree_by_id(wt_id).map(|wt| wt.active_terminal).unwrap_or(0);
                                terminals.get(&(wt_id, tab)).map(|emu| emu.scrollback())
                            }).unwrap_or(0);
                            app.text_selection = Some(selection::TextSelection {
                                anchor: pos,
                                current: pos,
                                active: true,
                                panel: selection::SelectionPanel::Terminal,
                                scroll_at_select: scrollback,
                            });
                        } else if hit_panel == Some(app::Panel::Right) {
                            app.text_selection = None;
                            // タブバー行のクリック → Tree/Changes 切替
                            if mouse.row == layout.right_top.y {
                                handle_right_panel_tab_click(
                                    app, diff_view, local_changes, layout.right_top, mouse.column,
                                );
                            } else {
                                // Treeモードならクリックでアイテム選択・ディレクトリ開閉
                                let mode = app
                                    .selected_worktree()
                                    .map(|wt| wt.right_panel_mode)
                                    .unwrap_or(app::RightPanelMode::Tree);
                                if mode == app::RightPanelMode::Tree {
                                    // タブバー (1行) 分を除いた領域を渡す
                                    let mut area = layout.right_top;
                                    area.y += 1;
                                    area.height = area.height.saturating_sub(1);
                                    if source_tree.click_at(area, mouse.row) {
                                        // ファイルクリック時はファイルを開く
                                        if !source_tree.current_is_dir() {
                                            if let Some(path) = source_tree.current_file_path() {
                                                ui::main_panel::open_file_tab(app, path);
                                            }
                                        }
                                    }
                                } else if mode == app::RightPanelMode::Diff {
                                    // Changes モード: タブバー (1行) を除いた領域を上下50%で分割
                                    let content_area = ratatui::prelude::Rect {
                                        x: layout.right_top.x,
                                        y: layout.right_top.y + 1,
                                        width: layout.right_top.width,
                                        height: layout.right_top.height.saturating_sub(1),
                                    };
                                    let half = content_area.height / 2;
                                    let pr_area_end = content_area.y + half;

                                    if mouse.row < pr_area_end {
                                        // PR Changes 領域クリック → カーソル移動＋diffタブを開く
                                        let inner_top = content_area.y + 1;
                                        if mouse.row >= inner_top {
                                            let clicked_row = (mouse.row - inner_top) as usize;
                                            if clicked_row < diff_view.files.len() {
                                                diff_view.selected_file = clicked_row;
                                                if let Some(wt) = app.selected_worktree_mut() {
                                                    wt.diff_focus = app::DiffFocus::PrDiff;
                                                }
                                                if let (Some(path), Some(content)) = (
                                                    diff_view.selected_path().map(|s| s.to_string()),
                                                    diff_view.selected_diff_content().map(|s| s.to_string()),
                                                ) {
                                                    ui::main_panel::open_diff_tab(app, &path, content);
                                                }
                                            }
                                        }
                                    } else {
                                        // Local Changes 領域クリック → カーソル移動＋diffタブを開く
                                        let local_top = pr_area_end + 1;
                                        if mouse.row >= local_top {
                                            let clicked_row = (mouse.row - local_top) as usize;
                                            if clicked_row < local_changes.files.len() {
                                                local_changes.selected_file = clicked_row;
                                                if let Some(wt) = app.selected_worktree_mut() {
                                                    wt.diff_focus = app::DiffFocus::LocalChanges;
                                                }
                                                if let (Some(path), Some(content)) = (
                                                    local_changes.selected_path().map(|s| s.to_string()),
                                                    local_changes.selected_diff_content().map(|s| s.to_string()),
                                                ) {
                                                    ui::main_panel::open_diff_tab(app, &path, content);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else if hit_panel == Some(app::Panel::Left) {
                            app.text_selection = None;
                            // ボーダー1行分を除いたコンテンツ領域の行を計算
                            let content_top = layout.left.y + 1;
                            if mouse.row >= content_top {
                                let clicked_row = (mouse.row - content_top) as usize;
                                let entries = LeftPanel::build_entries(&app.projects);
                                let target_index = left_panel.scroll_offset + clicked_row;
                                if target_index < entries.len() {
                                    left_panel.cursor_index = target_index;
                                    match &entries[target_index] {
                                        ui::left_panel::ListEntry::Worktree { project_index, worktree_index } => {
                                            let wt_id = (*project_index, *worktree_index);
                                            app.selected_worktree = Some(wt_id);
                                            app.focused_panel = app::Panel::Main;
                                            let wt_path = app.worktree_by_id(wt_id).unwrap().path.clone();
                                            let base = resolve_base_branch(&app.projects[wt_id.0].path, &app.projects[wt_id.0].name);
                                            source_tree.load(&wt_path);
                                            diff_view.load(&wt_path, &base);
                                            local_changes.load(&wt_path);
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
                                        ui::left_panel::ListEntry::Project { index } => {
                                            // プロジェクト行クリックで折りたたみ/展開
                                            app.projects[*index].collapsed = !app.projects[*index].collapsed;
                                            let new_entries = LeftPanel::build_entries(&app.projects);
                                            left_panel.clamp_cursor(new_entries.len());
                                        }
                                    }
                                }
                            }
                        } else if !file_selection_started {
                            app.text_selection = None;
                        }
                    }
                }
                MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                    if let Some(ref mut sel) = app.text_selection {
                        if sel.active {
                            let content_area = match sel.panel {
                                selection::SelectionPanel::Claude => app.claude_content_area,
                                selection::SelectionPanel::Terminal => app.terminal_content_area,
                                selection::SelectionPanel::File => app.file_content_area,
                            };
                            if let Some(ref area) = content_area {
                                sel.current = selection::screen_to_term(mouse.column, mouse.row, area);
                            }
                        }
                    }
                }
                MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                    if let Some(ref mut sel) = app.text_selection {
                        if sel.active {
                            sel.active = false;
                            if sel.is_empty() {
                                app.text_selection = None;
                            } else {
                                // マウスアップ時に自動コピー
                                copy_selection(app, claude_terms, terminals);
                            }
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
                                    if let Some(wt_id) = app.selected_worktree {
                                        let tab = app.selected_worktree()
                                            .map(|wt| wt.active_tab)
                                            .unwrap_or(0);
                                        if let Some(emu) = claude_terms.get_mut(&(wt_id, tab)) {
                                            let mouse_mode = emu.screen().mouse_protocol_mode();
                                            let alt_screen = emu.screen().alternate_screen();
                                            let scrollback_size = emu.screen().scrollback();
                                            // DEBUG: スクロールイベントの状態をログ出力
                                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                                .create(true).append(true)
                                                .open("/tmp/siki_scroll_debug.log")
                                            {
                                                let _ = writeln!(f,
                                                    "[scroll] dir={} mouse_mode={:?} alt_screen={} scrollback={} col={} row={}",
                                                    if is_up { "UP" } else { "DOWN" },
                                                    mouse_mode,
                                                    alt_screen,
                                                    scrollback_size,
                                                    mouse.column,
                                                    mouse.row,
                                                );
                                            }
                                            if mouse_mode != vt100::MouseProtocolMode::None {
                                                // PTY がマウスモードを有効にしている場合は転送
                                                let encoding = emu.screen().mouse_protocol_encoding();
                                                let (col, row) = if let Some(ca) = app.claude_content_area {
                                                    (
                                                        mouse.column.saturating_sub(ca.x) + 1,
                                                        mouse.row.saturating_sub(ca.y) + 1,
                                                    )
                                                } else {
                                                    (1, 1)
                                                };
                                                let bytes = terminal::mouse_scroll_to_bytes(is_up, col, row, encoding);
                                                if let Ok(mut f) = std::fs::OpenOptions::new()
                                                    .create(true).append(true)
                                                    .open("/tmp/siki_scroll_debug.log")
                                                {
                                                    let _ = writeln!(f,
                                                        "[scroll] -> PTY transfer: encoding={:?} pty_col={} pty_row={} bytes={:?}",
                                                        encoding, col, row, bytes,
                                                    );
                                                }
                                                let _ = emu.write(&bytes);
                                            } else {
                                                // マウスモード未有効: vt100 スクロールバックを操作
                                                let current = emu.scrollback();
                                                if is_up {
                                                    emu.set_scrollback(current.saturating_add(3));
                                                } else {
                                                    emu.set_scrollback(current.saturating_sub(3));
                                                }
                                            }
                                        }
                                    }
                                } else if app.show_grep_results_view {
                                    // Search タブ: grep 結果のカーソル移動
                                    let wt_path = app.selected_worktree().map(|wt| wt.path.clone());
                                    let row_count = if let Some(ref wt_path) = wt_path {
                                        ui::grep_view::build_display_rows(&app.grep_results, wt_path).len()
                                    } else {
                                        0
                                    };
                                    if is_up {
                                        app.grep_view_cursor = app.grep_view_cursor.saturating_sub(1);
                                    } else if app.grep_view_cursor < row_count.saturating_sub(1) {
                                        app.grep_view_cursor += 1;
                                    }
                                    adjust_grep_scroll(app);
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
                            Some(app::Panel::Terminal) => {
                                if let Some(wt_id) = app.selected_worktree {
                                    let active = app
                                        .selected_worktree()
                                        .map(|wt| wt.active_terminal)
                                        .unwrap_or(0);
                                    if let Some(emu) = terminals.get_mut(&(wt_id, active)) {
                                        let mouse_mode = emu.screen().mouse_protocol_mode();
                                        if mouse_mode != vt100::MouseProtocolMode::None {
                                            let encoding = emu.screen().mouse_protocol_encoding();
                                            let (col, row) = if let Some(ca) = app.terminal_content_area {
                                                (
                                                    mouse.column.saturating_sub(ca.x) + 1,
                                                    mouse.row.saturating_sub(ca.y) + 1,
                                                )
                                            } else {
                                                (1, 1)
                                            };
                                            let bytes = terminal::mouse_scroll_to_bytes(is_up, col, row, encoding);
                                            let _ = emu.write(&bytes);
                                        } else {
                                            let current = emu.scrollback();
                                            if is_up {
                                                emu.set_scrollback(current.saturating_add(3));
                                            } else {
                                                emu.set_scrollback(current.saturating_sub(3));
                                            }
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
        AppEvent::RemoteBranches { project_index, branches } => {
            if project_index == app.add_worktree_project_index {
                app.add_worktree_remote_branches = branches;
                app.add_worktree_loading = false;
                app.add_worktree_branch_cursor = 0;
            }
        }
        AppEvent::SessionUpdate { .. } => {
            // セッション状態の変化 — レジストリは broker 側で既に更新済み
        }
        AppEvent::SessionContext { worktree_id, summary } => {
            app.show_info("Launching Claude with context...".to_string());
            let prompt = if summary.is_empty() {
                "[Session Handoff] No context from previous session. Awaiting your instructions.".to_string()
            } else {
                format!(
                    "[Session Handoff]\n{}\n\n---\nSTOP. Do NOT take any action or run any tools. Wait for the operator's instructions.",
                    summary
                )
            };
            launch_claude_with_args(app, claude_terms, event_tx, worktree_id, &[&prompt]);
        }
        AppEvent::Paste(text) => {
            // siki.json 作成オーバーレイ表示中
            if app.show_siki_json_init_terminal {
                if let Some(emu) = siki_init_terminal.as_mut() {
                    let _ = emu.write(text.as_bytes());
                }
                return;
            }
            // スキル内容入力ポップアップ表示中
            if app.show_skill_edit_popup {
                app.skill_content_input.insert_str(app.skill_content_cursor, &text);
                app.skill_content_cursor += text.len();
                return;
            }
            // メッセージポップアップ: 入力欄に追加
            if app.show_message_popup {
                app.popup_input.push_str(&text);
                return;
            }
            // ワークツリー追加ポップアップ
            if app.show_add_worktree_popup {
                if app.add_worktree_mode == app::AddWorktreeMode::FromRemote {
                    app.add_worktree_branch_filter.push_str(&text);
                    app.add_worktree_branch_cursor = 0;
                } else {
                    app.add_worktree_input.push_str(&text);
                }
                return;
            }
            // grep ポップアップ
            if app.show_grep_popup {
                app.grep_input.push_str(&text);
                return;
            }
            // フォーカスに応じて Claude / 通常ターミナルへ転送
            if let Some(wt_id) = app.selected_worktree {
                if let Some(wt) = app.worktree_by_id(wt_id) {
                    if app.focused_panel == app::Panel::Terminal {
                        let active_terminal = wt.active_terminal;
                        if let Some(emu) = terminals.get_mut(&(wt_id, active_terminal)) {
                            let _ = emu.write(text.as_bytes());
                        }
                    } else {
                        let tab = wt.active_tab;
                        if tab < wt.claude_tabs {
                            if let Some(emu) = claude_terms.get_mut(&(wt_id, tab)) {
                                let _ = emu.write(text.as_bytes());
                            }
                        }
                    }
                }
            }
        }
        AppEvent::SkillRefineResult(result) => {
            app.skill_refining = false;
            match result {
                Ok(text) => {
                    app.skill_content_input = text.trim().to_string();
                    app.skill_content_cursor = app.skill_content_input.len();
                }
                Err(e) => {
                    app.show_error(format!("Claude 整形エラー: {}", e));
                }
            }
        }
        AppEvent::Tick => {
            app.clear_expired_status();
            if app.show_siki_json_init_terminal {
                app.siki_json_init_spinner = app.siki_json_init_spinner.wrapping_add(1);
            }
            if app.skill_refining {
                app.skill_refine_spinner = app.skill_refine_spinner.wrapping_add(1);
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
                display_name: None,
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

fn handle_rename_project_popup_key(app: &mut app::App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.show_rename_project_popup = false;
            app.rename_project_input.clear();
            app.rename_project_name = None;
            app.rename_worktree_target = None;
        }
        KeyCode::Enter => {
            let input = app.rename_project_input.trim().to_string();
            let display_name = if input.is_empty() { None } else { Some(input) };

            if let Some((pi, wi)) = app.rename_worktree_target {
                // worktree リネーム
                let names = app.projects.get(pi).map(|p| {
                    let wt_name = p.worktrees.get(wi).map(|wt| wt.name.clone());
                    (p.name.clone(), wt_name)
                });
                if let Some((project_name, Some(wt_name))) = names {
                    if let Some(wt) = app.projects[pi].worktrees.get_mut(wi) {
                        wt.display_name = display_name.clone();
                    }
                    if let Err(e) = config::save_worktree_display_name(
                        &project_name,
                        &wt_name,
                        display_name.as_deref(),
                    ) {
                        app.show_error(format!("表示名の保存に失敗: {}", e));
                    }
                }
            } else if let Some(project) = app.rename_project_name
                .as_deref()
                .and_then(|name| app.projects.iter_mut().find(|p| p.name == name))
            {
                // プロジェクトリネーム
                project.display_name = display_name.clone();
                if let Err(e) = config::save_project_display_name(
                    &project.name,
                    display_name.as_deref(),
                ) {
                    app.show_error(format!("表示名の保存に失敗: {}", e));
                }
            }

            app.show_rename_project_popup = false;
            app.rename_project_input.clear();
            app.rename_project_name = None;
            app.rename_worktree_target = None;
        }
        KeyCode::Char(c) if !c.is_control() && app.rename_project_input.chars().count() < 100 => {
            app.rename_project_input.push(c);
        }
        KeyCode::Backspace => {
            app.rename_project_input.pop();
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
            app.add_worktree_branch_filter.clear();
            app.add_worktree_mode = app::AddWorktreeMode::NewBranch;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            app.add_worktree_mode = app.add_worktree_mode.next();
            // FromRemote に切り替えたとき、ブランチ一覧が未取得なら非同期取得
            if app.add_worktree_mode == app::AddWorktreeMode::FromRemote
                && app.add_worktree_remote_branches.is_empty()
                && !app.add_worktree_loading
            {
                app.add_worktree_loading = true;
                let pi = app.add_worktree_project_index;
                let project_path = app.projects[pi].path.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let branches = tokio::task::spawn_blocking(move || {
                        git::WorktreeManager::list_remote_branches(&project_path)
                            .unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();
                    let _ = tx.send(event::AppEvent::RemoteBranches {
                        project_index: pi,
                        branches,
                    });
                });
            }
        }
        KeyCode::Enter => {
            match app.add_worktree_mode {
                app::AddWorktreeMode::NewBranch | app::AddWorktreeMode::FromBase => {
                    let branch = app.add_worktree_input.trim().to_string();
                    if branch.is_empty() {
                        return;
                    }
                    let start_point = if app.add_worktree_mode == app::AddWorktreeMode::FromBase {
                        Some(app.add_worktree_base_branch.clone())
                    } else {
                        None
                    };
                    finalize_add_worktree(
                        app, terminals, event_tx, shell,
                        &branch,
                        start_point.as_deref(),
                    );
                }
                app::AddWorktreeMode::FromRemote => {
                    let filtered: Vec<String> = app
                        .add_worktree_remote_branches
                        .iter()
                        .filter(|b| {
                            app.add_worktree_branch_filter.is_empty()
                                || b.contains(&app.add_worktree_branch_filter)
                        })
                        .cloned()
                        .collect();
                    if let Some(remote_branch) = filtered.get(app.add_worktree_branch_cursor) {
                        // origin/feature-x → feature-x
                        let local_branch = remote_branch
                            .find('/')
                            .map(|i| &remote_branch[i + 1..])
                            .unwrap_or(remote_branch)
                            .to_string();
                        let start_point = remote_branch.clone();
                        finalize_add_worktree(
                            app, terminals, event_tx, shell,
                            &local_branch,
                            Some(&start_point),
                        );
                    }
                }
            }
        }
        KeyCode::Up => {
            if app.add_worktree_mode == app::AddWorktreeMode::FromRemote {
                app.add_worktree_branch_cursor =
                    app.add_worktree_branch_cursor.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if app.add_worktree_mode == app::AddWorktreeMode::FromRemote {
                let filtered_count = app
                    .add_worktree_remote_branches
                    .iter()
                    .filter(|b| {
                        app.add_worktree_branch_filter.is_empty()
                            || b.contains(&app.add_worktree_branch_filter)
                    })
                    .count();
                if filtered_count > 0 {
                    app.add_worktree_branch_cursor =
                        (app.add_worktree_branch_cursor + 1).min(filtered_count - 1);
                }
            }
        }
        KeyCode::Char(c) => {
            if app.add_worktree_mode == app::AddWorktreeMode::FromRemote {
                app.add_worktree_branch_filter.push(c);
                app.add_worktree_branch_cursor = 0;
            } else {
                app.add_worktree_input.push(c);
            }
        }
        KeyCode::Backspace => {
            if app.add_worktree_mode == app::AddWorktreeMode::FromRemote {
                app.add_worktree_branch_filter.pop();
                app.add_worktree_branch_cursor = 0;
            } else {
                app.add_worktree_input.pop();
            }
        }
        _ => {}
    }
}

/// worktree 作成の共通処理（モード問わず）
fn finalize_add_worktree(
    app: &mut app::App,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    branch: &str,
    start_point: Option<&str>,
) {
    let pi = app.add_worktree_project_index;
    let wt_name = app.add_worktree_name.clone();

    let project_name = app.projects[pi].name.clone();
    let project_path = app.projects[pi].path.clone();
    let wt_path = config::worktree_path(&project_name, &wt_name);

    let shared_dirs = config::load_effective_shared_dirs(&project_name);

    if let Err(e) = git::WorktreeManager::create_worktree_from_ref(
        &project_path,
        &wt_path,
        branch,
        start_point,
        &shared_dirs,
    ) {
        app.show_error(format!("worktree の作成に失敗: {}", e));
        app.show_add_worktree_popup = false;
        app.add_worktree_input.clear();
        app.add_worktree_branch_filter.clear();
        app.add_worktree_mode = app::AddWorktreeMode::NewBranch;
        return;
    }

    // メモリ上に worktree を追加
    app.projects[pi].worktrees.push(app::Worktree {
        name: wt_name.clone(),
        display_name: None,
        branch: branch.to_string(),
        path: wt_path.clone(),
        status: app::WorktreeStatus::Idle,
        chat_history: Vec::new(),
        open_files: Vec::new(),
        active_tab: 0,
        claude_tabs: 0,
        right_panel_mode: app::RightPanelMode::Tree,
        diff_focus: app::DiffFocus::PrDiff,
        active_terminal: 0,
        chat_scroll_offset: 0,
        claude_scroll_offsets: HashMap::new(),
        pr_title: None,
        claude_session_id: None,
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

    // setup スクリプトがあれば実行（siki.json 優先、なければ project.json）
    let wi = app.projects[pi].worktrees.len() - 1;
    let wt_id = (pi, wi);
    let project_name = &app.projects[pi].name;
    if let Some(siki_json) = config::load_effective_siki_json(&project_path, project_name) {
        if let Some(ref setup_script) = siki_json.scripts.setup {
            run_siki_script(
                app, terminals, event_tx, shell,
                wt_id, setup_script, &wt_name, &wt_path,
            );
        }
    }

    app.show_add_worktree_popup = false;
    app.add_worktree_input.clear();
    app.add_worktree_branch_filter.clear();
    app.add_worktree_mode = app::AddWorktreeMode::NewBranch;
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
        // 新規セッションを起動（前回の session_id があれば再開を試行）
        let (path, resume_id) = match app.worktree_by_id(wt_id) {
            Some(wt) => (wt.path.clone(), wt.claude_session_id.clone()),
            None => return,
        };

        let spawn_result = claude::ClaudeSession::spawn(&path, event_tx.clone(), wt_id, resume_id.as_deref()).await;

        // resume 失敗時は新規セッションでフォールバック
        let spawn_result = match (&spawn_result, &resume_id) {
            (Err(_), Some(_)) => {
                if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                    wt.claude_session_id = None;
                }
                claude::ClaudeSession::spawn(&path, event_tx.clone(), wt_id, None).await
            }
            _ => spawn_result,
        };

        match spawn_result {
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

    // Escape で選択クリア（選択中なら PTY には送らない）
    if key.code == KeyCode::Esc && app.text_selection.is_some() {
        app.text_selection = None;
        return;
    }
    // キー入力時は選択をクリア（表示だけ消す）
    if app.text_selection.is_some() && !key.modifiers.contains(KeyModifiers::SHIFT) {
        app.text_selection = None;
    }

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

    // Ctrl+g で grep 検索を開く
    if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.show_grep_popup = true;
        app.grep_input.clear();
        app.grep_results.clear();
        app.grep_cursor = 0;
        return;
    }

    if !has_terminal {
        // ターミナル未作成時: Ctrl+t で新規作成、それ以外は無視
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
        // Ctrl+] で次のタブへ循環切替
        // 注: Ctrl+[ は ESC と同じバイトシーケンスのため使用不可
        if key.code == KeyCode::Char(']') {
            let existing = existing_terminal_tabs(terminals, wt_id);
            if existing.len() > 1 {
                if let Some(cur_pos) = existing.iter().position(|&t| t == active_tab) {
                    let next_pos = (cur_pos + 1) % existing.len();
                    if let Some(wt) = app.worktree_by_id_mut(wt_id) {
                        wt.active_terminal = existing[next_pos];
                    }
                }
            }
            return;
        }
        // Ctrl+t で新規タブ
        if key.code == KeyCode::Char('t') {
            let next_tab = (0..ui::MAX_TERMINAL_TABS).find(|i| !terminals.contains_key(&(wt_id, *i)));
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
            // キー入力時はスクロールバックを最新に戻す
            emu.set_scrollback(0);
            let _ = emu.write(&bytes);
        }
    }
}

/// 指定 worktree に存在するターミナルタブのインデックス一覧を返す
fn existing_terminal_tabs(
    terminals: &HashMap<TerminalKey, terminal::TerminalEmulator>,
    wt_id: app::WorktreeId,
) -> Vec<usize> {
    (0..ui::MAX_TERMINAL_TABS)
        .filter(|i| terminals.contains_key(&(wt_id, *i)))
        .collect()
}

/// ターミナルタイトルバーのタブクリックを処理する
fn handle_terminal_tab_click(
    app: &mut app::App,
    terminals: &HashMap<TerminalKey, terminal::TerminalEmulator>,
    term_area: ratatui::prelude::Rect,
    click_x: u16,
) {
    let Some(wt_id) = app.selected_worktree else {
        return;
    };
    let existing = existing_terminal_tabs(terminals, wt_id);
    if existing.is_empty() {
        return;
    }
    // ボーダー左端(1) + プレフィックス からタブ部分開始
    let tabs_start_x = term_area.x + 1 + ui::TERMINAL_TITLE_PREFIX.len() as u16;
    let tabs_end_x = tabs_start_x + (existing.len() as u16 * ui::TERMINAL_TAB_WIDTH as u16);
    if click_x < tabs_start_x || click_x >= tabs_end_x {
        return;
    }
    let offset = (click_x - tabs_start_x) as usize;
    let tab_index = offset / ui::TERMINAL_TAB_WIDTH;
    if let Some(&target_tab) = existing.get(tab_index) {
        if let Some(wt) = app.worktree_by_id_mut(wt_id) {
            wt.active_terminal = target_tab;
        }
    }
}

/// 選択範囲のテキストをクリップボードにコピーする
fn copy_selection(
    app: &app::App,
    claude_terms: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
) {
    let sel = match app.text_selection.as_ref() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    let text = match sel.panel {
        selection::SelectionPanel::File => {
            let (start, end) = sel.normalize();
            extract_file_selection(app, sel, &start, &end)
        }
        _ => {
            let wt_id = match app.selected_worktree {
                Some(id) => id,
                None => return,
            };
            let emu = match sel.panel {
                selection::SelectionPanel::Claude => {
                    let tab = app.worktree_by_id(wt_id).map(|wt| wt.active_tab).unwrap_or(0);
                    claude_terms.get_mut(&(wt_id, tab))
                }
                selection::SelectionPanel::Terminal => {
                    let tab = app.worktree_by_id(wt_id).map(|wt| wt.active_terminal).unwrap_or(0);
                    terminals.get_mut(&(wt_id, tab))
                }
                selection::SelectionPanel::File => unreachable!(),
            };
            emu.and_then(|emu| {
                // 選択時のスクロール位置に戻してテキストを抽出し、元に戻す
                let current_scroll = emu.scrollback();
                emu.set_scrollback(sel.scroll_at_select);
                let (start, end) = sel.normalize();
                let text = selection::extract_text(emu.screen(), &start, &end);
                emu.set_scrollback(current_scroll);
                Some(text)
            })
        }
    };
    if let Some(text) = text {
        if !text.is_empty() {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(text);
            }
        }
    }
}

/// ファイルビューの選択範囲からテキストを抽出する
fn extract_file_selection(
    app: &app::App,
    sel: &selection::TextSelection,
    start: &selection::TermPos,
    end: &selection::TermPos,
) -> Option<String> {
    let wt = app.selected_worktree()?;
    let has_search = app.show_grep_results_view;
    let offset = if has_search { wt.claude_tabs + 1 } else { wt.claude_tabs };
    let file_index = wt.active_tab.checked_sub(offset)?;
    let file = wt.open_files.get(file_index)?;

    // 選択時のスクロールオフセットを使用して絶対行番号に変換
    let start_line = sel.scroll_at_select + start.row as usize;
    let end_line = sel.scroll_at_select + end.row as usize;

    // 行番号プレフィックス "1234 " = 5文字分をスキップ
    const LINE_NUM_WIDTH: u16 = 5;

    let mut result = String::new();
    for line_idx in start_line..=end_line {
        let spans = file.highlighted.get(line_idx)?;
        let line_text: String = spans.iter().map(|(_, _, _, t)| t.as_str()).collect();

        if start_line == end_line {
            // 単一行選択
            let s = start.col.saturating_sub(LINE_NUM_WIDTH) as usize;
            let e = (end.col.saturating_sub(LINE_NUM_WIDTH) as usize + 1).min(line_text.len());
            if s < line_text.len() {
                result.push_str(&line_text[s..e]);
            }
        } else if line_idx == start_line {
            let s = start.col.saturating_sub(LINE_NUM_WIDTH) as usize;
            if s < line_text.len() {
                result.push_str(&line_text[s..]);
            }
            result.push('\n');
        } else if line_idx == end_line {
            let e = (end.col.saturating_sub(LINE_NUM_WIDTH) as usize + 1).min(line_text.len());
            result.push_str(&line_text[..e]);
        } else {
            result.push_str(&line_text);
            result.push('\n');
        }
    }
    Some(result)
}

/// 右パネルのタブバー (Tree | Changes) のクリックを処理する
fn handle_right_panel_tab_click(
    app: &mut app::App,
    diff_view: &mut ui::diff_view::DiffView,
    local_changes: &mut ui::diff_view::LocalChangesView,
    right_top_area: ratatui::prelude::Rect,
    click_x: u16,
) {
    let Some(wt_id) = app.selected_worktree else {
        return;
    };
    // ratatui 0.30 Tabs: "Tree|Changes" (パディングなし)
    // "Tree" = 4文字, "|" = 1文字, "Changes" = 7文字
    let tab_x = click_x.saturating_sub(right_top_area.x);
    let new_mode = if tab_x < 4 {
        app::RightPanelMode::Tree
    } else if tab_x >= 5 {
        app::RightPanelMode::Diff
    } else {
        // divider 上のクリックは無視
        return;
    };
    if let Some(wt) = app.worktree_by_id_mut(wt_id) {
        if wt.right_panel_mode != new_mode {
            wt.right_panel_mode = new_mode;
            // Diff モードに切り替えた時は最新の diff を取得
            if new_mode == app::RightPanelMode::Diff {
                let wt_path = wt.path.clone();
                let base = resolve_base_branch(&app.projects[wt_id.0].path, &app.projects[wt_id.0].name);
                diff_view.load(&wt_path, &base);
                local_changes.load(&wt_path);
            }
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
    local_changes: &mut ui::diff_view::LocalChangesView,
    terminals: &mut HashMap<TerminalKey, terminal::TerminalEmulator>,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
    shell: &str,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    // 一部ターミナルでは Shift+l が KeyCode::Char('l') + SHIFT として送られるため、
    // 大文字に正規化する
    let code = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() => {
            KeyCode::Char(c.to_ascii_uppercase())
        }
        other => other,
    };

    let entries = LeftPanel::build_entries(&app.projects);

    match code {
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
                app.add_worktree_branch_filter.clear();
                app.add_worktree_branch_cursor = 0;
                app.add_worktree_remote_branches.clear();
                app.add_worktree_loading = false;
                app.add_worktree_mode = app::AddWorktreeMode::NewBranch;

                // base_branch を解決: siki.json > config.toml > "origin/main"
                let project_name = &app.projects[pi].name;
                app.add_worktree_base_branch = resolve_base_branch(project_path, project_name);

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

                let project_name = &app.projects[pi].name;
                if let Some(siki_json) = config::load_effective_siki_json(&project_path, project_name) {
                    if let Some(ref run_script) = siki_json.scripts.run {
                        run_siki_script(
                            app, terminals, event_tx, shell,
                            wt_id, run_script, &wt_name, &wt_path,
                        );
                        app.focused_panel = app::Panel::Terminal;
                    } else {
                        app.show_info("run スクリプトが定義されていません".to_string());
                    }
                } else {
                    app.show_info("スクリプト設定が見つかりません（siki.json または project.json）".to_string());
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
        KeyCode::Char('S') => {
            // siki.json 作成（プロジェクト行 or worktree 行で有効）
            let project_index = match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Project { index }) => Some(*index),
                Some(ui::left_panel::ListEntry::Worktree { project_index, .. }) => {
                    Some(*project_index)
                }
                None => None,
            };
            if let Some(pi) = project_index {
                let project_path = &app.projects[pi].path;
                if config::siki_json_exists(project_path) {
                    app.show_info("siki.json は既に存在します".to_string());
                } else {
                    app.siki_json_confirm_project_path = Some(project_path.clone());
                    app.show_siki_json_confirm = true;
                }
            }
        }
        KeyCode::Char('K') => {
            // スキル作成ポップアップを開く
            let project_index = match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Project { index }) => Some(*index),
                Some(ui::left_panel::ListEntry::Worktree { project_index, .. }) => Some(*project_index),
                None => None,
            };
            if let Some(pi) = project_index {
                let project_name = &app.projects[pi].name;
                app.skill_project_name = Some(project_name.clone());
                // 既存スキルを読み込む
                let skills_dir = config::project_skills_dir(project_name);
                let mut items = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            let skill_file = path.join("SKILL.md");
                            if skill_file.exists() {
                                let name = path.file_name()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let content = std::fs::read_to_string(&skill_file).unwrap_or_default();
                                items.push((name, content));
                            }
                        }
                    }
                }
                items.sort_by(|a, b| a.0.cmp(&b.0));
                app.skill_list_items = items;
                app.skill_list_cursor = 0;
                app.show_skill_list = true;
            }
        }
        KeyCode::Char('L') => {
            // シンボリックリンク設定ポップアップを開く
            let project_index = match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Project { index }) => Some(*index),
                Some(ui::left_panel::ListEntry::Worktree { project_index, .. }) => Some(*project_index),
                None => None,
            };
            if let Some(pi) = project_index {
                let project_name = &app.projects[pi].name;
                let project_path = &app.projects[pi].path;

                // .gitignore から候補を発見
                let candidates = config::discover_symlink_candidates(project_path);

                // 現在の設定を取得
                let current = config::load_effective_shared_dirs(project_name);

                // マージ: 候補 + 現在有効だが候補にないもの（実在するディレクトリのみ）
                let mut items: Vec<(String, bool)> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for c in &candidates {
                    seen.insert(c.clone());
                    items.push((c.clone(), current.contains(c)));
                }
                for c in &current {
                    if seen.insert(c.clone()) && project_path.join(c).is_dir() {
                        items.push((c.clone(), true));
                    }
                }

                app.symlink_project_name = Some(project_name.clone());
                app.symlink_items = items;
                app.symlink_cursor = 0;
                app.symlink_input_mode = false;
                app.symlink_input.clear();
                app.show_symlink_settings = true;
            }
        }
        KeyCode::Char('R') => {
            // 表示名変更ポップアップを開く（プロジェクト行 or worktree 行）
            match left_panel.current_entry(&entries) {
                Some(ui::left_panel::ListEntry::Project { index }) => {
                    let pi = *index;
                    let current_display = app.projects[pi]
                        .display_name
                        .clone()
                        .unwrap_or_default();
                    app.rename_project_name = Some(app.projects[pi].name.clone());
                    app.rename_worktree_target = None;
                    app.rename_project_input = current_display;
                    app.show_rename_project_popup = true;
                }
                Some(ui::left_panel::ListEntry::Worktree { project_index, worktree_index }) => {
                    let pi = *project_index;
                    let wi = *worktree_index;
                    let current_display = app.projects[pi].worktrees[wi]
                        .display_name
                        .clone()
                        .unwrap_or_default();
                    app.rename_project_name = Some(app.projects[pi].name.clone());
                    app.rename_worktree_target = Some((pi, wi));
                    app.rename_project_input = current_display;
                    app.show_rename_project_popup = true;
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
                let base = resolve_base_branch(&app.projects[wt_id.0].path, &app.projects[wt_id.0].name);
                source_tree.load(&wt_path);
                diff_view.load(&wt_path, &base);
                local_changes.load(&wt_path);
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

    // grep 結果ビュー表示中
    if app.show_grep_results_view {
        handle_grep_view_key(app, claude_terms, key);
        return;
    }

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
        KeyCode::Char('y') => {
            // path:line をクリップボードにコピー
            if let Some(loc) = ui::main_panel::current_file_location(app) {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(&loc);
                    app.show_info(format!("コピー: {}", loc));
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

/// スキル名入力ポップアップのキー処理
fn handle_skill_name_popup_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => {
            app.show_skill_name_popup = false;
            app.skill_name_input.clear();
        }
        KeyCode::Enter => {
            let skill_name = app.skill_name_input.trim().to_string();
            if skill_name.is_empty() {
                return;
            }
            app.show_skill_name_popup = false;
            // 既存スキルがあれば内容を読み込む
            if let Some(project_name) = app.skill_project_name.as_deref() {
                let skill_file = config::project_skills_dir(project_name)
                    .join(&skill_name)
                    .join("SKILL.md");
                app.skill_content_input = std::fs::read_to_string(&skill_file).unwrap_or_default();
            }
            app.skill_content_cursor = app.skill_content_input.len();
            app.show_skill_edit_popup = true;
        }
        KeyCode::Backspace => {
            app.skill_name_input.pop();
        }
        KeyCode::Char(c) => {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                app.skill_name_input.push(c);
            }
        }
        _ => {}
    }
}

/// スキル内容入力ポップアップの保存処理
fn save_skill_content(app: &mut app::App) {
    let skill_name = app.skill_name_input.trim().to_string();
    if let Some(project_name) = app.skill_project_name.as_deref() {
        let skill_dir = config::project_skills_dir(project_name).join(&skill_name);
        let _ = std::fs::create_dir_all(&skill_dir);
        let _ = std::fs::write(skill_dir.join("SKILL.md"), &app.skill_content_input);
        for project in &app.projects {
            if project.name == project_name {
                for wt in &project.worktrees {
                    hooks::sync_project_skills(&wt.path, project_name);
                }
                break;
            }
        }
        app.show_info(format!("スキル /{} を保存しました", skill_name));
    }
    app.show_skill_edit_popup = false;
    app.skill_content_input.clear();
    app.skill_content_cursor = 0;
    app.skill_name_input.clear();
}

/// スキル内容入力ポップアップのキー処理
fn handle_skill_edit_popup_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
    event_tx: &tokio::sync::mpsc::UnboundedSender<event::AppEvent>,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    // 整形中はキー入力を無視
    if app.skill_refining {
        return;
    }

    // Shift+Enter → Ctrl+J として来る → 改行
    if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.skill_content_input.insert(app.skill_content_cursor, '\n');
        app.skill_content_cursor += 1;
        return;
    }

    // Ctrl+S で保存
    if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
        save_skill_content(app);
        return;
    }

    // Ctrl+R で Claude 整形
    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.skill_content_input.trim().is_empty() {
            app.show_info("整形するテキストがありません".to_string());
            return;
        }
        app.skill_refining = true;
        app.skill_refine_spinner = 0;
        let content = app.skill_content_input.clone();
        let skill_name = app.skill_name_input.clone();
        let tx = event_tx.clone();
        tokio::spawn(async move {
            let prompt = format!(
                "以下のテキストを Claude Code の skill ファイル（SKILL.md）として適切なフォーマットに整形してください。\n\
                スキル名: /{}\n\n\
                整形ルール:\n\
                - Markdown形式で構造化する\n\
                - 説明、動作手順、注意事項などのセクションを適切に追加する\n\
                - 元のテキストの意図を保持する\n\
                - 整形結果のみを出力し、説明や前置きは不要\n\n\
                ---\n{}", skill_name, content
            );
            let result = tokio::process::Command::new("claude")
                .args(["-p", &prompt])
                .output()
                .await;
            match result {
                Ok(output) if output.status.success() => {
                    let text = String::from_utf8_lossy(&output.stdout).to_string();
                    let _ = tx.send(event::AppEvent::SkillRefineResult(Ok(text)));
                }
                Ok(output) => {
                    let err = String::from_utf8_lossy(&output.stderr).to_string();
                    let _ = tx.send(event::AppEvent::SkillRefineResult(Err(err)));
                }
                Err(e) => {
                    let _ = tx.send(event::AppEvent::SkillRefineResult(Err(e.to_string())));
                }
            }
        });
        return;
    }

    // Enter: 修飾キーなしで保存、それ以外（Shift等）は改行
    if key.code == KeyCode::Enter {
        if key.modifiers == KeyModifiers::NONE {
            save_skill_content(app);
        } else {
            app.skill_content_input.insert(app.skill_content_cursor, '\n');
            app.skill_content_cursor += 1;
        }
        return;
    }

    // Ctrl/Super/Meta 付きはスキップ（Shiftは通す）
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META) {
        return;
    }

    let s = &mut app.skill_content_input;
    let cur = &mut app.skill_content_cursor;

    match key.code {
        KeyCode::Esc => {
            app.show_skill_edit_popup = false;
            s.clear();
            *cur = 0;
            app.skill_name_input.clear();
        }
        KeyCode::Backspace => {
            if *cur > 0 {
                // 前の文字境界を探す
                let prev = s[..*cur].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
                s.drain(prev..*cur);
                *cur = prev;
            }
        }
        KeyCode::Delete => {
            if *cur < s.len() {
                let next = s[*cur..].char_indices().nth(1).map(|(i, _)| *cur + i).unwrap_or(s.len());
                s.drain(*cur..next);
            }
        }
        KeyCode::Left => {
            if *cur > 0 {
                *cur = s[..*cur].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
            }
        }
        KeyCode::Right => {
            if *cur < s.len() {
                *cur = s[*cur..].char_indices().nth(1).map(|(i, _)| *cur + i).unwrap_or(s.len());
            }
        }
        KeyCode::Home => {
            // 行頭へ
            let line_start = s[..*cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
            *cur = line_start;
        }
        KeyCode::End => {
            // 行末へ
            let line_end = s[*cur..].find('\n').map(|i| *cur + i).unwrap_or(s.len());
            *cur = line_end;
        }
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_control() => {
            // Shift+Enter が制御文字として来た場合 → 改行
            s.insert(*cur, '\n');
            *cur += 1;
        }
        KeyCode::Char(c) => {
            s.insert(*cur, c);
            *cur += c.len_utf8();
        }
        _ => {}
    }
}

/// スキル一覧ポップアップのキー処理
fn handle_symlink_settings_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;
    if app.symlink_input_mode {
        match key.code {
            KeyCode::Esc => {
                app.symlink_input_mode = false;
                app.symlink_input.clear();
            }
            KeyCode::Enter => {
                let name = app.symlink_input.trim().to_string();
                if !name.is_empty() && !app.symlink_items.iter().any(|(n, _)| n == &name) {
                    app.symlink_items.push((name, true));
                    app.symlink_cursor = app.symlink_items.len() - 1;
                }
                app.symlink_input_mode = false;
                app.symlink_input.clear();
            }
            KeyCode::Backspace => {
                app.symlink_input.pop();
            }
            KeyCode::Char(c) => {
                app.symlink_input.push(c);
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc => {
            app.show_symlink_settings = false;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.symlink_items.is_empty() {
                app.symlink_cursor = (app.symlink_cursor + 1).min(app.symlink_items.len() - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.symlink_cursor = app.symlink_cursor.saturating_sub(1);
        }
        KeyCode::Char(' ') => {
            if let Some(item) = app.symlink_items.get_mut(app.symlink_cursor) {
                item.1 = !item.1;
            }
        }
        KeyCode::Char('a') => {
            app.symlink_input_mode = true;
            app.symlink_input.clear();
        }
        KeyCode::Char('d') => {
            if !app.symlink_items.is_empty() {
                app.symlink_items.remove(app.symlink_cursor);
                if app.symlink_cursor >= app.symlink_items.len() && app.symlink_cursor > 0 {
                    app.symlink_cursor -= 1;
                }
            }
        }
        KeyCode::Enter => {
            // 保存＆既存 worktree に適用
            let enabled: Vec<String> = app
                .symlink_items
                .iter()
                .filter(|(_, checked)| *checked)
                .map(|(name, _)| name.clone())
                .collect();

            if let Some(ref project_name) = app.symlink_project_name {
                if let Err(e) = config::save_project_shared_dirs(project_name, enabled.clone()) {
                    app.show_error(format!("shared_dirs の保存に失敗: {}", e));
                } else {
                    // 既存 worktree にシンボリックリンクを適用
                    let mut applied = 0;
                    for project in &app.projects {
                        if project.name == *project_name {
                            for wt in &project.worktrees {
                                for dir_name in &enabled {
                                    let _ = git::setup_shared_dir(
                                        &project.path,
                                        &wt.path,
                                        dir_name,
                                    );
                                }
                                applied += 1;
                            }
                            break;
                        }
                    }
                    app.show_info(format!(
                        "Symlink 設定を保存しました ({} dirs, {} worktrees)",
                        enabled.len(),
                        applied
                    ));
                }
            }
            app.show_symlink_settings = false;
        }
        _ => {}
    }
}

fn handle_skill_list_key(
    app: &mut app::App,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => {
            app.show_skill_list = false;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.skill_list_items.is_empty() && app.skill_list_cursor < app.skill_list_items.len() - 1 {
                app.skill_list_cursor += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.skill_list_cursor = app.skill_list_cursor.saturating_sub(1);
        }
        KeyCode::Enter => {
            // 既存スキルを編集
            if let Some((name, content)) = app.skill_list_items.get(app.skill_list_cursor) {
                app.skill_name_input = name.clone();
                app.skill_content_input = content.clone();
                app.skill_content_cursor = content.len();
                app.show_skill_list = false;
                app.show_skill_edit_popup = true;
            }
        }
        KeyCode::Char('n') => {
            // 新規作成 → スキル名入力へ
            app.show_skill_list = false;
            app.skill_name_input.clear();
            app.show_skill_name_popup = true;
        }
        KeyCode::Char('d') => {
            // スキル削除
            if let Some((name, _)) = app.skill_list_items.get(app.skill_list_cursor) {
                if let Some(project_name) = app.skill_project_name.as_deref() {
                    let path = config::project_skills_dir(project_name).join(name);
                    let _ = std::fs::remove_dir_all(&path);
                    // worktreeのシンボリックリンクも削除
                    for project in &app.projects {
                        if project.name == project_name {
                            for wt in &project.worktrees {
                                let link = wt.path.join(".claude").join("skills").join(name);
                                if link.is_symlink() {
                                    let _ = std::fs::remove_file(&link);
                                }
                            }
                            break;
                        }
                    }
                    app.skill_list_items.remove(app.skill_list_cursor);
                    if app.skill_list_cursor >= app.skill_list_items.len() && app.skill_list_cursor > 0 {
                        app.skill_list_cursor -= 1;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Grep ポップアップのキー処理
/// grep 結果ビュー（中央ペイン）のキー処理
fn handle_grep_view_key(
    app: &mut app::App,
    claude_terms: &mut HashMap<(app::WorktreeId, usize), terminal::TerminalEmulator>,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::KeyCode;

    // grep_rows を都度構築（カーソル位置の解決用）
    let wt_path = app.selected_worktree().map(|wt| wt.path.clone());
    let grep_rows = if let Some(ref wt_path) = wt_path {
        ui::grep_view::build_display_rows(&app.grep_results, wt_path)
    } else {
        Vec::new()
    };

    match key.code {
        KeyCode::Esc | KeyCode::Char('w') => {
            app.show_grep_results_view = false;
            // Search タブが消えるのでタブ位置を調整
            if let Some(wt) = app.selected_worktree_mut() {
                if wt.active_tab >= wt.claude_tabs {
                    wt.active_tab = wt.claude_tabs.saturating_sub(1);
                }
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.grep_view_cursor < grep_rows.len().saturating_sub(1) {
                app.grep_view_cursor += 1;
            }
            // スクロール追従（描画時に計算するので scroll_offset を更新）
            adjust_grep_scroll(app);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.grep_view_cursor = app.grep_view_cursor.saturating_sub(1);
            adjust_grep_scroll(app);
        }
        KeyCode::Enter => {
            // 選択行のファイルを開く
            if let Some((path, line)) =
                ui::grep_view::location_at(&grep_rows, app.grep_view_cursor, &app.grep_results)
            {
                app.show_grep_results_view = false;
                ui::main_panel::open_file_tab(app, path);
                if let Some(wt) = app.selected_worktree_mut() {
                    let file_index = wt.active_tab.saturating_sub(wt.claude_tabs);
                    if let Some(file) = wt.open_files.get_mut(file_index) {
                        let target = line.saturating_sub(1);
                        if target < file.highlighted.len() {
                            file.cursor_line = target;
                        }
                    }
                }
            }
        }
        KeyCode::Char('y') => {
            // path:line をクリップボードにコピー
            if let Some(loc) = ui::grep_view::location_string_at(&grep_rows, app.grep_view_cursor)
            {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(&loc);
                    app.show_info(format!("コピー: {}", loc));
                }
            }
        }
        KeyCode::Char('s') => {
            // path:line を Claude Code に送信
            if let Some(loc) = ui::grep_view::location_string_at(&grep_rows, app.grep_view_cursor)
            {
                if let Some(wt_id) = app.selected_worktree {
                    if let Some(emu) = claude_terms.get_mut(&(wt_id, 0)) {
                        let msg = format!("{}\n", loc);
                        if let Err(e) = emu.write(msg.as_bytes()) {
                            app.show_error(format!("Claude への送信に失敗: {}", e));
                        } else {
                            app.show_info(format!("送信: {}", loc));
                        }
                    }
                }
            }
        }
        KeyCode::Char('g') => {
            // 再検索（grep ポップアップを開く）
            app.show_grep_results_view = false;
            app.show_grep_popup = true;
            app.grep_results.clear();
            app.grep_cursor = 0;
        }
        _ => {}
    }
}

fn adjust_grep_scroll(app: &mut app::App) {
    // 簡易スクロール追従: 概算の表示高さで調整
    let visible = 20_usize; // 近似値、実際の描画時に再調整される
    if app.grep_view_cursor >= app.grep_view_scroll + visible {
        app.grep_view_scroll = app.grep_view_cursor - visible + 1;
    }
    if app.grep_view_cursor < app.grep_view_scroll {
        app.grep_view_scroll = app.grep_view_cursor;
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
            // 検索を実行して結果を中央ペインに表示
            app.run_grep();
            if !app.grep_results.is_empty() {
                app.show_grep_popup = false;
                app.show_grep_results_view = true;
                app.grep_view_cursor = 0;
                app.grep_view_scroll = 0;
                app.focused_panel = app::Panel::Main;
                // Search タブに切り替え（claude_tabs の直後）
                if let Some(wt) = app.selected_worktree_mut() {
                    wt.active_tab = wt.claude_tabs;
                }
            }
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

                // archive スクリプトがあれば実行（siki.json 優先、なければ project.json）
                let project_name = &app.projects[pi].name;
                if let Some(siki_json) = config::load_effective_siki_json(&project_path, project_name) {
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
///
/// レジストリに加え、既に Claude タブが開いているかもチェック（hook 未到着対策）
fn has_active_sessions(
    session_registry: &Option<Arc<Mutex<session::SessionRegistry>>>,
    app: &app::App,
    wt_id: app::WorktreeId,
) -> bool {
    // 既に Claude タブが開いていれば、hook 到着に関係なくアクティブ
    if let Some(wt) = app.worktree_by_id(wt_id) {
        if wt.claude_tabs > 0 {
            return true;
        }
    }
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
    let project_name = app.projects[wt_id.0].name.clone();

    // worktree に siki 用 hook を注入
    let sock = config::sock_path();
    if let Err(e) = hooks::ensure_hooks_configured(&project_path, &sock, &project_name) {
        app.show_error(format!("hook 注入に失敗: {}", e));
    }
    let claude_idx = app
        .worktree_by_id(wt_id)
        .map(|wt| wt.claude_tabs)
        .unwrap_or(0);
    let size = (80, 24);

    let result = terminal::TerminalEmulator::with_args(
        "claude",
        args,
        &project_path,
        size,
        event_tx.clone(),
        wt_id,
        CLAUDE_TAB_BASE + claude_idx,
    );

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

    // Escape で選択クリア（選択中なら PTY には送らない）
    if key.code == KeyCode::Esc && app.text_selection.is_some() {
        app.text_selection = None;
        return;
    }

    // キー入力時は選択をクリア（表示だけ消す）
    if app.text_selection.is_some() && !key.modifiers.contains(KeyModifiers::SHIFT) {
        app.text_selection = None;
    }

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

    // Ctrl+g で grep 検索を開く
    if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.show_grep_popup = true;
        app.grep_input.clear();
        app.grep_results.clear();
        app.grep_cursor = 0;
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

    // Shift+Up/Down/PageUp/PageDown: スクロール
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let is_up = matches!(key.code, KeyCode::Up | KeyCode::PageUp);
                let count = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) { 10 } else { 1 };
                if let Some(emu) = claude_terms.get_mut(&(wt_id, active_tab)) {
                    let mouse_mode = emu.screen().mouse_protocol_mode();
                    if mouse_mode != vt100::MouseProtocolMode::None {
                        let encoding = emu.screen().mouse_protocol_encoding();
                        for _ in 0..count {
                            let bytes = terminal::mouse_scroll_to_bytes(is_up, 1, 1, encoding);
                            let _ = emu.write(&bytes);
                        }
                    } else {
                        let current = emu.scrollback();
                        if is_up {
                            emu.set_scrollback(current.saturating_add(count));
                        } else {
                            emu.set_scrollback(current.saturating_sub(count));
                        }
                    }
                }
                return;
            }
            _ => {}
        }
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
    local_changes: &mut ui::diff_view::LocalChangesView,
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
                    // Diff モードに切り替えた時は最新の diff を取得
                    if let Some(wt_id) = app.selected_worktree {
                        if let Some(wt) = app.worktree_by_id(wt_id) {
                            if wt.right_panel_mode == app::RightPanelMode::Diff {
                                let wt_path = wt.path.clone();
                                let base = resolve_base_branch(&app.projects[wt_id.0].path, &app.projects[wt_id.0].name);
                                diff_view.load(&wt_path, &base);
                                local_changes.load(&wt_path);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        app::RightPanelMode::Diff => {
            let focus = app
                .selected_worktree()
                .map(|wt| wt.diff_focus)
                .unwrap_or(app::DiffFocus::PrDiff);

            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    match focus {
                        app::DiffFocus::PrDiff => {
                            // 末尾なら LocalChanges へ遷移
                            if !diff_view.files.is_empty()
                                && diff_view.selected_file >= diff_view.files.len() - 1
                            {
                                if let Some(wt) = app.selected_worktree_mut() {
                                    wt.diff_focus = app::DiffFocus::LocalChanges;
                                }
                                local_changes.selected_file = 0;
                            } else {
                                diff_view.select_next();
                            }
                        }
                        app::DiffFocus::LocalChanges => {
                            local_changes.select_next();
                        }
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    match focus {
                        app::DiffFocus::PrDiff => {
                            diff_view.select_prev();
                        }
                        app::DiffFocus::LocalChanges => {
                            // 先頭なら PrDiff へ遷移
                            if local_changes.selected_file == 0 {
                                if let Some(wt) = app.selected_worktree_mut() {
                                    wt.diff_focus = app::DiffFocus::PrDiff;
                                }
                                if !diff_view.files.is_empty() {
                                    diff_view.selected_file = diff_view.files.len() - 1;
                                }
                            } else {
                                local_changes.select_prev();
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    // 選択中のファイルの diff を中央パネルに開く
                    let (path, content) = match focus {
                        app::DiffFocus::PrDiff => (
                            diff_view.selected_path().map(|s| s.to_string()),
                            diff_view.selected_diff_content().map(|s| s.to_string()),
                        ),
                        app::DiffFocus::LocalChanges => (
                            local_changes.selected_path().map(|s| s.to_string()),
                            local_changes.selected_diff_content().map(|s| s.to_string()),
                        ),
                    };
                    if let (Some(path), Some(content)) = (path, content) {
                        ui::main_panel::open_diff_tab(app, &path, content);
                        app.focused_panel = app::Panel::Main;
                    }
                }
                KeyCode::Char('t') => {
                    ui::right_panel::toggle_mode(app);
                }
                KeyCode::Tab => {
                    // Tab で上下パネル切替
                    if let Some(wt) = app.selected_worktree_mut() {
                        wt.diff_focus = match wt.diff_focus {
                            app::DiffFocus::PrDiff => app::DiffFocus::LocalChanges,
                            app::DiffFocus::LocalChanges => app::DiffFocus::PrDiff,
                        };
                    }
                }
                _ => {}
            }
        }
    }
}

/// プロジェクトパスとプロジェクト名から base_branch を解決する
fn resolve_base_branch(project_path: &std::path::Path, project_name: &str) -> String {
    config::load_effective_siki_json(project_path, project_name)
        .and_then(|sj| sj.base_branch)
        .or_else(|| {
            config::load_config(&config::default_config_path())
                .ok()
                .and_then(|c| c.siki.base_branch)
        })
        .unwrap_or_else(|| "origin/main".to_string())
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
                base_branch: None,
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path: "/tmp/test-project".to_string(),
                display_name: None,
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
                base_branch: None,
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path,
                display_name: None,
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

    fn test_broker_db() -> Arc<Mutex<rusqlite::Connection>> {
        Arc::new(Mutex::new(db::init(std::path::Path::new(":memory:")).unwrap()))
    }

    // --- グローバルキーの状態遷移テスト ---

    #[tokio::test]
    async fn test_key_q_quits() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('?'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Tab)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::BackTab)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(shift_key(KeyCode::Tab)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
                &mut local_changes,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
                &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(ctrl_key('\\')),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Tick,
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
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
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
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
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
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
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('j'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('i'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('t'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            Some(&layout),
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
            &test_broker_db(),
        )
        .await;
        assert!(app.show_add_worktree_popup);
        assert_eq!(app.add_worktree_project_index, 0);
    }

    #[tokio::test]
    async fn test_a_key_shows_worktree_popup_without_siki_json() {
        let dir = tempfile::TempDir::new().unwrap();
        // siki.json を作成しない → worktree 追加ポップアップが表示される
        let config = sample_config_with_path(dir.path().to_string_lossy().to_string());
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('a'))),
            None,
            &None,
            &test_broker_db(),
        )
        .await;
        assert!(!app.show_siki_json_confirm);
        assert!(app.show_add_worktree_popup);
    }

    #[tokio::test]
    async fn test_shift_s_key_shows_siki_json_confirm() {
        let dir = tempfile::TempDir::new().unwrap();
        // siki.json を作成しない → S で作成確認ポップアップが表示される
        let config = sample_config_with_path(dir.path().to_string_lossy().to_string());
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('S'))),
            None,
            &None,
            &test_broker_db(),
        )
        .await;
        assert!(app.show_siki_json_confirm);
    }

    #[tokio::test]
    async fn test_add_worktree_popup_esc_cancels() {
        let config = sample_config();
        let mut app = app::App::new(&config);
        let mut left_panel = LeftPanel::new();
        let mut source_tree = SourceTree::new();
        let mut diff_view = DiffView::new();
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
                &mut local_changes,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
                &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Backspace)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
            &test_broker_db(),
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
                base_branch: None,
            },
            projects: vec![ProjectConfig {
                name: "test-project".to_string(),
                path: project_path.to_string_lossy().to_string(),
                display_name: None,
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('A'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Esc)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
                &mut local_changes,
                &mut sessions,
                &mut terminals,
                &mut claude_terms,
                &mut siki_init_terminal,
                &tx,
                "/bin/sh",
                event::AppEvent::Key(key(KeyCode::Char(c))),
                None,
                &None,
                &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Enter)),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Key(key(KeyCode::Char('q'))),
            None,
            &None,
            &test_broker_db(),
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
        let mut local_changes = ui::diff_view::LocalChangesView::new();
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
            &mut local_changes,
            &mut sessions,
            &mut terminals,
            &mut claude_terms,
            &mut siki_init_terminal,
            &tx,
            "/bin/sh",
            event::AppEvent::Mouse(mouse),
            None,
            &None,
            &test_broker_db(),
        )
        .await;
        assert_eq!(app.focused_panel, app::Panel::Left);
    }
}

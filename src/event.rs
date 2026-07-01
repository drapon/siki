use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind, MouseEvent};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::app::WorktreeId;

/// アプリケーション全体の統合イベント型
#[derive(Debug)]
pub enum AppEvent {
    /// キーボード入力
    Key(KeyEvent),
    /// マウス入力
    Mouse(MouseEvent),
    /// Claude Code からの出力
    ClaudeOutput {
        worktree_id: WorktreeId,
        /// 送信元 ClaudeSession の生成時点で不変の一意id。worktree 削除の reindex で
        /// worktree_id 自体がシフトしていても、sessions 内の現在のキーを再特定するために使う
        session_id: u64,
        event: ClaudeStreamEvent,
    },
    /// Claude Code の応答完了
    ClaudeComplete {
        worktree_id: WorktreeId,
        session_id: u64,
    },
    /// Claude Code のエラー
    ClaudeError {
        worktree_id: WorktreeId,
        session_id: u64,
        error: String,
    },
    /// ターミナル（PTY）からの出力
    TerminalOutput {
        worktree_id: WorktreeId,
        tab_index: usize,
        terminal_id: u64,
        data: Vec<u8>,
    },
    /// ターミナル（PTY）プロセスが終了した
    TerminalExited {
        worktree_id: WorktreeId,
        tab_index: usize,
        terminal_id: u64,
    },
    /// GitHub PR 情報取得完了
    PrInfo {
        worktree_id: WorktreeId,
        /// fetch 開始時点でキャプチャした worktree 名。worktree/project 削除の reindex で
        /// worktree_id 自体がシフトしていても、無関係な worktree へ誤って書き込まないよう
        /// 受信側で現在の名前と照合するために使う
        worktree_name: String,
        info: Option<crate::app::PrInfo>,
    },
    /// 定期 tick（UI リフレッシュ）
    Tick,
    /// ウィンドウリサイズ
    Resize(u16, u16),
    /// セッション状態の変化（broker から）
    SessionUpdate {
        session_id: String,
        state: crate::session::SessionState,
    },
    /// 前セッションのコンテキストサマリー取得完了 → Claude を起動する
    SessionContext {
        worktree_id: WorktreeId,
        summary: String,
    },
    /// リモートブランチ一覧取得完了
    RemoteBranches {
        project_index: usize,
        branches: Vec<String>,
    },
    /// 全ブランチ一覧取得完了（ベースブランチ選択用）
    AllBranches {
        project_index: usize,
        branches: Vec<String>,
    },
    /// スキル整形完了（claude -c の結果）
    SkillRefineResult(Result<String, String>),
    /// コンテキスト整形完了（claude -p の結果）
    ContextRefineResult(Result<String, String>),
    /// URL からのコンテキスト取得完了（title, content）
    ContextUrlFetchResult(Result<(String, String), String>),
    /// ペースト入力（ブラケットペーストモード）
    Paste(String),
    /// Grep 検索結果
    GrepResults(Vec<crate::app::GrepResult>),
    /// Changes の再読み込み要求（broker 経由の refresh イベント）
    RefreshChanges,
    /// History タブでの手動サマリ生成完了
    HistorySummaryGenerated {
        session_id: String,
        summary: String,
    },
    /// git fetch 完了
    FetchComplete {
        project_index: usize,
        result: Result<String, String>,
    },
    /// worktree の git 削除がバックグラウンドで完了した
    WorktreeRemoveCompleted {
        worktree_name: String,
        result: Result<(), String>,
    },
}

/// Claude Code ストリームイベント型
#[derive(Debug, Clone)]
pub enum ClaudeStreamEvent {
    Init { session_id: String },
    ContentDelta { text: String },
    ToolUse { tool: String, input: serde_json::Value },
    Result { text: String },
    Error { message: String },
}

/// 複数の非同期イベントソースを統合するハンドラー
pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<AppEvent>,
    tx: mpsc::UnboundedSender<AppEvent>,
}

impl EventHandler {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        // キーボードイベント読み取りタスク
        let key_tx = tx.clone();
        tokio::spawn(async move {
            let tick_rate = Duration::from_millis(100);
            loop {
                if event::poll(tick_rate).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            // KeyEventKind::Press のみ処理（リリースイベントを無視）
                            if key.kind == KeyEventKind::Press {
                                if key_tx.send(AppEvent::Key(key)).is_err() {
                                    break;
                                }
                            }
                        }
                        Ok(Event::Mouse(mouse)) => {
                            use crossterm::event::{MouseButton, MouseEventKind};
                            match mouse.kind {
                                MouseEventKind::Down(_)
                                | MouseEventKind::Drag(MouseButton::Left)
                                | MouseEventKind::Moved
                                | MouseEventKind::Up(MouseButton::Left)
                                | MouseEventKind::ScrollUp
                                | MouseEventKind::ScrollDown => {
                                    if key_tx.send(AppEvent::Mouse(mouse)).is_err() {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(Event::Resize(w, h)) => {
                            if key_tx.send(AppEvent::Resize(w, h)).is_err() {
                                break;
                            }
                        }
                        Ok(Event::Paste(text)) => {
                            if key_tx.send(AppEvent::Paste(text)).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                } else {
                    // タイムアウト = Tick
                    if key_tx.send(AppEvent::Tick).is_err() {
                        break;
                    }
                }
            }
        });

        Self { rx, tx }
    }

    /// イベントチャネルの送信側を取得（Claude/PTY タスクから使用）
    pub fn sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    /// 次のイベントを待受する
    pub async fn next(&mut self) -> Result<AppEvent> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("イベントチャネルが閉じられました"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_event_variants() {
        // AppEvent の各バリアントが構築できることを確認
        let _key = AppEvent::Key(KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let _tick = AppEvent::Tick;
        let _resize = AppEvent::Resize(80, 24);
        let _claude_output = AppEvent::ClaudeOutput {
            worktree_id: (0, 0),
            session_id: 1,
            event: ClaudeStreamEvent::ContentDelta {
                text: "hello".to_string(),
            },
        };
        let _claude_complete = AppEvent::ClaudeComplete {
            worktree_id: (0, 0),
            session_id: 1,
        };
        let _claude_error = AppEvent::ClaudeError {
            worktree_id: (0, 0),
            session_id: 1,
            error: "test error".to_string(),
        };
        let _terminal_output = AppEvent::TerminalOutput {
            worktree_id: (0, 0),
            tab_index: 0,
            terminal_id: 0,
            data: vec![0x41],
        };
        let _mouse = AppEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
    }

    #[test]
    fn test_claude_stream_event_variants() {
        let _init = ClaudeStreamEvent::Init {
            session_id: "abc-123".to_string(),
        };
        let _delta = ClaudeStreamEvent::ContentDelta {
            text: "hello".to_string(),
        };
        let _tool = ClaudeStreamEvent::ToolUse {
            tool: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/test"}),
        };
        let _result = ClaudeStreamEvent::Result {
            text: "done".to_string(),
        };
        let _error = ClaudeStreamEvent::Error {
            message: "failed".to_string(),
        };
    }

    #[tokio::test]
    async fn test_event_handler_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

        tx.send(AppEvent::Tick).unwrap();
        tx.send(AppEvent::Resize(120, 40)).unwrap();

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, AppEvent::Tick));

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, AppEvent::Resize(120, 40)));
    }
}

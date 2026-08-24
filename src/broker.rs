use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::db;
use crate::event::AppEvent;
use crate::session::{HookEvent, SessionRegistry, SessionState};

/// Broker: Unix ソケットで hook イベントを受信し、セッションレジストリを更新する
pub struct Broker {
    listener: UnixListener,
    registry: Arc<Mutex<SessionRegistry>>,
    db: Arc<Mutex<Connection>>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    /// 会話ログ保存済みのセッションID（重複保存防止）
    saved_sessions: Arc<Mutex<HashSet<String>>>,
}

impl Broker {
    /// ソケットファイルを作成して Broker を初期化する
    pub fn new(
        sock_path: &Path,
        registry: Arc<Mutex<SessionRegistry>>,
        db: Arc<Mutex<Connection>>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Result<Self> {
        // 既存のソケットファイルがあれば削除（前回の異常終了対策）
        if sock_path.exists() {
            std::fs::remove_file(sock_path)?;
        }

        // 親ディレクトリを確保
        if let Some(parent) = sock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(sock_path)?;

        Ok(Self {
            listener,
            registry,
            db,
            event_tx,
            saved_sessions: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// 接続を受け付けてイベントを処理するループ（tokio::spawn で起動する）
    pub async fn run(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, _)) => {
                    let registry = Arc::clone(&self.registry);
                    let db = Arc::clone(&self.db);
                    let event_tx = self.event_tx.clone();

                    let saved_sessions = Arc::clone(&self.saved_sessions);
                    tokio::spawn(async move {
                        // 1接続 = 1行のJSONメッセージ — 読み取り後すぐに接続を閉じる
                        // （nc クライアントが接続終了を待ってブロックするのを防ぐ）
                        let line = {
                            let reader = tokio::io::BufReader::new(stream);
                            let mut lines = reader.lines();
                            lines.next_line().await
                        };
                        // ↑ reader + stream がここで drop → nc は即座に EOF を受け取り終了

                        let line = match line {
                            Ok(Some(l)) => l,
                            _ => return,
                        };

                        let hook_event = match serde_json::from_str::<HookEvent>(&line) {
                            Ok(e) => e,
                            Err(_) => return,
                        };

                        let is_refresh = matches!(hook_event, HookEvent::Refresh { .. });
                        let is_idle = matches!(hook_event, HookEvent::Idle { .. });
                        let session_id = hook_event.session_id().to_string();

                        // SQLite にも書き込む
                        Self::sync_to_db(&db, &hook_event);

                        let changed = {
                            let mut reg = registry.lock().unwrap();
                            reg.handle_event(hook_event)
                        };

                        // Idle 時に会話ログを非同期保存
                        if is_idle {
                            let already_saved = {
                                let set = saved_sessions.lock().unwrap();
                                set.contains(&session_id)
                            };
                            if !already_saved {
                                let db2 = Arc::clone(&db);
                                let saved2 = Arc::clone(&saved_sessions);
                                let sid = session_id.clone();
                                tokio::spawn(async move {
                                    save_conversation_log(&db2, &sid, &saved2).await;
                                });
                            }
                        }

                        // Refresh イベントは Changes の再読み込みを通知
                        if is_refresh {
                            let _ = event_tx.send(AppEvent::RefreshChanges);
                            return;
                        }

                        if changed {
                            let state = {
                                let reg = registry.lock().unwrap();
                                reg.get(&session_id)
                                    .map(|s| s.state)
                                    .unwrap_or(SessionState::Idle)
                            };
                            let _ = event_tx.send(AppEvent::SessionUpdate {
                                session_id,
                                state,
                            });
                        }
                    });
                }
                Err(e) => {
                    eprintln!("broker: 接続受付エラー: {}", e);
                }
            }
        }
    }

    /// hook イベントを SQLite に同期する
    fn sync_to_db(db: &Arc<Mutex<Connection>>, event: &HookEvent) {
        let conn = match db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        match event {
            HookEvent::Register { session_id, cwd, role } => {
                let (project, worktree) = crate::session::guess_names_from_cwd(cwd);
                let _ = db::upsert_session(&conn, session_id, role, &worktree, &project, cwd, "idle");
            }
            HookEvent::Working { session_id, activity } => {
                let _ = db::update_session_state(&conn, session_id, "working");
                // activity が同梱されていれば永続化（来ない場合は直前の値を保持）
                if let Some(act) = activity {
                    let _ = db::update_session_activity(&conn, session_id, act);
                }
            }
            HookEvent::Waiting { session_id } => {
                let _ = db::update_session_state(&conn, session_id, "waiting");
            }
            HookEvent::Idle { session_id } => {
                let _ = db::update_session_state(&conn, session_id, "idle");
            }
            HookEvent::Dead { session_id } => {
                let _ = db::update_session_state(&conn, session_id, "dead");
            }
            HookEvent::Refresh { .. } => {}
        }
    }
}

/// セッションの会話ログをJSONLから読み込んでSQLiteに保存する
async fn save_conversation_log(
    db: &Arc<Mutex<Connection>>,
    session_id: &str,
    saved_sessions: &Arc<Mutex<HashSet<String>>>,
) {
    // DB からセッション情報を取得
    let session_info = {
        let conn = match db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut stmt = match conn.prepare(
            "SELECT cwd, worktree_name, project_name, claude_session_id FROM sessions WHERE session_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let result: Result<(String, String, String, Option<String>), _> = stmt.query_row(
            rusqlite::params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );
        match result {
            Ok(info) => info,
            Err(_) => return,
        }
    };

    let (cwd, worktree_name, project_name, claude_session_id) = session_info;

    // claude_session_id が必要（JSONLファイル名の特定に使う）
    let claude_sid = match claude_session_id {
        Some(ref id) if !id.is_empty() => id.clone(),
        _ => return,
    };

    // JSONLファイルを読み込む（ブロッキングI/Oなので spawn_blocking）
    let cwd_path = std::path::PathBuf::from(&cwd);
    let parsed = tokio::task::spawn_blocking(move || {
        let encoded = crate::claude_history::encode_path(&cwd_path);
        let jsonl_path = crate::claude_history::claude_projects_dir()
            .join(&encoded)
            .join(format!("{}.jsonl", claude_sid));

        if !jsonl_path.exists() {
            return None;
        }
        crate::claude_history::parse_jsonl(&jsonl_path)
    })
    .await;

    let history = match parsed {
        Ok(Some(h)) => h,
        _ => return,
    };

    // メッセージをJSON配列にシリアライズ
    let messages_json: Vec<serde_json::Value> = history
        .messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect();
    let messages_str = serde_json::to_string(&messages_json).unwrap_or_default();

    // SQLiteに保存
    {
        let conn = match db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = db::upsert_conversation_log(
            &conn,
            session_id,
            &worktree_name,
            &project_name,
            if history.git_branch.is_empty() {
                None
            } else {
                Some(&history.git_branch)
            },
            &messages_str,
        );
    }

    // 保存済みとしてマーク
    if let Ok(mut set) = saved_sessions.lock() {
        set.insert(session_id.to_string());
    }

    // 非同期でサマリ生成
    let db2 = Arc::clone(db);
    let sid = session_id.to_string();
    tokio::spawn(async move {
        generate_summary(&db2, &sid, &messages_str).await;
    });
}

/// Claude CLI を使って会話のサマリを生成し、DBに保存する
async fn generate_summary(
    db: &Arc<Mutex<Connection>>,
    session_id: &str,
    messages_json: &str,
) {
    // メッセージが空なら何もしない
    let messages: Vec<serde_json::Value> = match serde_json::from_str(messages_json) {
        Ok(m) => m,
        Err(_) => return,
    };
    if messages.is_empty() {
        return;
    }

    // サマリ用のテキストを構築（トークン節約のため要約向けに整形）
    let mut summary_input = String::new();
    for msg in &messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        // 各メッセージの先頭200文字に制限（大量のコードブロック回避）
        let truncated: String = content.chars().take(200).collect();
        summary_input.push_str(&format!("[{}] {}\n", role, truncated));
    }

    // 入力が大きすぎる場合は先頭と末尾のみ
    if summary_input.len() > 4000 {
        let chars: Vec<char> = summary_input.chars().collect();
        let head: String = chars[..1500].iter().collect();
        let tail: String = chars[chars.len() - 1500..].iter().collect();
        summary_input = format!("{}\n...(中略)...\n{}", head, tail);
    }

    let prompt = format!(
        "以下はClaude Codeの会話ログです。このworktreeで何をやったかを3-5行で簡潔に要約してください。技術的な詳細（変更したファイル、実装内容、修正したバグ等）を含めてください。\n\n{}",
        summary_input
    );

    // claude --print で非同期実行
    let result = tokio::process::Command::new("claude")
        .args(["--print", "-p", &prompt])
        .output()
        .await;

    let summary = match result {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => return, // 失敗してもsummaryがNULLのまま残るだけ
    };

    if summary.is_empty() {
        return;
    }

    // DBに保存
    let conn = match db.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = db::update_conversation_log_summary(&conn, session_id, &summary);
}

/// 起動時に未保存のJSONLファイルを回収してSQLiteに保存する（非同期）
///
/// conversation_logs テーブルに存在しないJSONLファイルを検出し、バックグラウンドで保存する。
pub async fn sync_unsaved_conversation_logs(
    db: Arc<Mutex<Connection>>,
    worktrees: Vec<(String, String, String)>, // (worktree_name, project_name, cwd)
) {
    for (worktree_name, project_name, cwd) in worktrees {
        let cwd_path = std::path::PathBuf::from(&cwd);
        let wt_name = worktree_name.clone();
        let proj_name = project_name.clone();
        let db = Arc::clone(&db);

        // JSONLファイル一覧を取得（ブロッキングI/O）
        let unsaved = tokio::task::spawn_blocking({
            let db = Arc::clone(&db);
            let wt_name = wt_name.clone();
            let proj_name = proj_name.clone();
            move || {
                let encoded = crate::claude_history::encode_path(&cwd_path);
                let project_dir = crate::claude_history::claude_projects_dir().join(&encoded);

                if !project_dir.is_dir() {
                    return Vec::new();
                }

                // DB に保存済みのセッションIDを取得
                let saved_ids: HashSet<String> = {
                    let conn = match db.lock() {
                        Ok(c) => c,
                        Err(_) => return Vec::new(),
                    };
                    db::get_saved_conversation_session_ids(&conn, &wt_name, &proj_name)
                        .unwrap_or_default()
                        .into_iter()
                        .collect()
                };

                // 未保存のJSONLファイルを特定
                let mut unsaved = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&project_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            let sid = crate::claude_history::session_id_from_path(&path);
                            if !saved_ids.contains(&sid) {
                                if let Some(history) = crate::claude_history::parse_jsonl(&path) {
                                    unsaved.push((sid, history));
                                }
                            }
                        }
                    }
                }
                unsaved
            }
        })
        .await
        .unwrap_or_default();

        // 未保存分をDBに保存
        if !unsaved.is_empty() {
            let conn = match db.lock() {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (sid, history) in unsaved {
                let messages_json: Vec<serde_json::Value> = history
                    .messages
                    .iter()
                    .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
                    .collect();
                let messages_str = serde_json::to_string(&messages_json).unwrap_or_default();
                let _ = db::upsert_conversation_log(
                    &conn,
                    &sid,
                    &wt_name,
                    &proj_name,
                    if history.git_branch.is_empty() {
                        None
                    } else {
                        Some(&history.git_branch)
                    },
                    &messages_str,
                );
            }
        }
    }
}

/// ソケットファイルを削除する（アプリ終了時に呼ぶ）
pub fn cleanup_socket(sock_path: &Path) {
    let _ = std::fs::remove_file(sock_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;

    fn test_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(db::init(Path::new(":memory:")).unwrap()))
    }

    #[tokio::test]
    async fn test_broker_accepts_register() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");
        let registry = Arc::new(Mutex::new(SessionRegistry::new()));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let broker = Broker::new(&sock_path, Arc::clone(&registry), test_db(), event_tx).unwrap();
        let broker_handle = tokio::spawn(broker.run());

        // クライアント側: register イベントを送信
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (_, mut writer) = tokio::io::split(stream);
        use tokio::io::AsyncWriteExt;
        let msg = r#"{"event":"register","session_id":"test-1","cwd":"/tmp","role":"default"}"#;
        writer.write_all(msg.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();

        // イベント受信を待つ
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();

        match event {
            AppEvent::SessionUpdate { session_id, state } => {
                assert_eq!(session_id, "test-1");
                assert_eq!(state, SessionState::Idle);
            }
            _ => panic!("expected SessionUpdate event"),
        }

        // レジストリにも登録されている
        let reg = registry.lock().unwrap();
        assert!(reg.get("test-1").is_some());

        broker_handle.abort();
    }

    #[tokio::test]
    async fn test_broker_handles_state_change() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("test2.sock");
        let registry = Arc::new(Mutex::new(SessionRegistry::new()));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // 先にセッションを登録しておく
        {
            let mut reg = registry.lock().unwrap();
            reg.register("sess-a".into(), "/tmp".into(), "default".into());
        }

        let broker = Broker::new(&sock_path, Arc::clone(&registry), test_db(), event_tx).unwrap();
        let broker_handle = tokio::spawn(broker.run());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // working イベントを送信
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (_, mut writer) = tokio::io::split(stream);
        use tokio::io::AsyncWriteExt;
        let msg = r#"{"event":"working","session_id":"sess-a"}"#;
        writer.write_all(msg.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();

        match event {
            AppEvent::SessionUpdate { session_id, state } => {
                assert_eq!(session_id, "sess-a");
                assert_eq!(state, SessionState::Working);
            }
            _ => panic!("expected SessionUpdate with Working state"),
        }

        broker_handle.abort();
    }

    #[tokio::test]
    async fn test_broker_persists_activity() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("test_act.sock");
        let registry = Arc::new(Mutex::new(SessionRegistry::new()));
        let db = test_db();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // 事前にセッションを registry / DB に登録（SessionStart 相当）
        {
            let mut reg = registry.lock().unwrap();
            reg.register("sess-x".into(), "/tmp".into(), "default".into());
        }
        {
            let conn = db.lock().unwrap();
            db::upsert_session(&conn, "sess-x", "default", "wt", "proj", "/tmp", "idle").unwrap();
        }

        let broker =
            Broker::new(&sock_path, Arc::clone(&registry), Arc::clone(&db), event_tx).unwrap();
        let broker_handle = tokio::spawn(broker.run());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // activity 付き working を送信
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (_, mut writer) = tokio::io::split(stream);
        use tokio::io::AsyncWriteExt;
        let msg = r#"{"event":"working","session_id":"sess-x","activity":"Edit: a.rs"}"#;
        writer.write_all(msg.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();

        // SessionUpdate 受信 = registry/DB 更新完了
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();

        // registry に activity が反映されている
        assert_eq!(
            registry.lock().unwrap().get("sess-x").unwrap().activity.as_deref(),
            Some("Edit: a.rs")
        );

        // DB の activity 列にも永続化されている
        let act: Option<String> = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT activity FROM sessions WHERE session_id = ?1",
                rusqlite::params!["sess-x"],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(act.as_deref(), Some("Edit: a.rs"));

        broker_handle.abort();
    }

    #[tokio::test]
    async fn test_broker_handles_refresh() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("test3.sock");
        let registry = Arc::new(Mutex::new(SessionRegistry::new()));
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let broker = Broker::new(&sock_path, Arc::clone(&registry), test_db(), event_tx).unwrap();
        let broker_handle = tokio::spawn(broker.run());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // refresh イベントを送信
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (_, mut writer) = tokio::io::split(stream);
        use tokio::io::AsyncWriteExt;
        let msg = r#"{"event":"refresh","session_id":"sess-r"}"#;
        writer.write_all(msg.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(event, AppEvent::RefreshChanges));

        broker_handle.abort();
    }

    #[test]
    fn test_cleanup_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("cleanup.sock");
        std::fs::write(&sock_path, "dummy").unwrap();
        assert!(sock_path.exists());
        cleanup_socket(&sock_path);
        assert!(!sock_path.exists());
    }

    #[test]
    fn test_cleanup_socket_nonexistent() {
        // 存在しないファイルの削除でパニックしない
        cleanup_socket(Path::new("/tmp/nonexistent_siki_test.sock"));
    }
}

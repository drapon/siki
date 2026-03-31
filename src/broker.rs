use anyhow::Result;
use rusqlite::Connection;
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

                    tokio::spawn(async move {
                        let reader = tokio::io::BufReader::new(stream);
                        let mut lines = reader.lines();

                        // 1接続 = 1行のJSONメッセージ
                        if let Ok(Some(line)) = lines.next_line().await {
                            if let Ok(hook_event) = serde_json::from_str::<HookEvent>(&line) {
                                let session_id = hook_event.session_id().to_string();

                                // SQLite にも書き込む
                                Self::sync_to_db(&db, &hook_event);

                                let changed = {
                                    let mut reg = registry.lock().unwrap();
                                    reg.handle_event(hook_event)
                                };
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
                            }
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
            HookEvent::Working { session_id } => {
                let _ = db::update_session_state(&conn, session_id, "working");
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

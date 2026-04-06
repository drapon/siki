use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// データベースを初期化し、テーブルを作成する
pub fn init(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("SQLite を開けません: {}", db_path.display()))?;

    // WAL モードを有効化（並行アクセス対応）
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            role TEXT NOT NULL DEFAULT 'default',
            worktree_name TEXT NOT NULL DEFAULT 'unknown',
            project_name TEXT NOT NULL DEFAULT 'unknown',
            cwd TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL DEFAULT 'idle',
            summary TEXT,
            claude_session_id TEXT,
            last_heartbeat INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_session TEXT NOT NULL,
            to_session TEXT,
            to_worktree TEXT,
            to_project TEXT,
            content TEXT NOT NULL,
            message_type TEXT NOT NULL DEFAULT 'message',
            metadata TEXT,
            created_at INTEGER NOT NULL,
            read_at INTEGER
        );
        ",
    )?;

    // マイグレーション: claude_session_id カラムが無ければ追加
    let _ = conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;",
    );

    Ok(conn)
}

/// セッションを登録または更新する
pub fn upsert_session(
    conn: &Connection,
    session_id: &str,
    role: &str,
    worktree_name: &str,
    project_name: &str,
    cwd: &str,
    state: &str,
) -> Result<()> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO sessions (session_id, role, worktree_name, project_name, cwd, state, last_heartbeat, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
           state = ?6,
           last_heartbeat = ?7",
        rusqlite::params![session_id, role, worktree_name, project_name, cwd, state, now],
    )?;
    Ok(())
}

/// セッション状態を更新する
pub fn update_session_state(conn: &Connection, session_id: &str, state: &str) -> Result<()> {
    let now = now_unix();
    conn.execute(
        "UPDATE sessions SET state = ?1, last_heartbeat = ?2 WHERE session_id = ?3",
        rusqlite::params![state, now, session_id],
    )?;
    Ok(())
}

/// セッションの summary を更新する
pub fn update_session_summary(
    conn: &Connection,
    session_id: &str,
    summary: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET summary = ?1 WHERE session_id = ?2",
        rusqlite::params![summary, session_id],
    )?;
    Ok(())
}

/// Claude Code のセッション ID を保存する
pub fn update_claude_session_id(
    conn: &Connection,
    session_id: &str,
    claude_session_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET claude_session_id = ?1 WHERE session_id = ?2",
        rusqlite::params![claude_session_id, session_id],
    )?;
    Ok(())
}

/// 指定 worktree の最新の claude_session_id を取得する
///
/// 同じ worktree/project で最も新しく作成されたセッションの claude_session_id を返す。
pub fn get_latest_claude_session_id(
    conn: &Connection,
    worktree_name: &str,
    project_name: &str,
) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT claude_session_id FROM sessions
         WHERE worktree_name = ?1 AND project_name = ?2
           AND claude_session_id IS NOT NULL
         ORDER BY last_heartbeat DESC
         LIMIT 1",
        rusqlite::params![worktree_name, project_name],
        |row| row.get(0),
    );
    match result {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// セッション一覧を取得する
pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, role, worktree_name, project_name, cwd, state, summary
         FROM sessions WHERE state != 'dead'
         ORDER BY project_name, worktree_name, role",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SessionRow {
            session_id: row.get(0)?,
            role: row.get(1)?,
            worktree_name: row.get(2)?,
            project_name: row.get(3)?,
            cwd: row.get(4)?,
            state: row.get(5)?,
            summary: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// メッセージを保存する
pub fn insert_message(
    conn: &Connection,
    from_session: &str,
    to_session: Option<&str>,
    to_worktree: Option<&str>,
    to_project: Option<&str>,
    content: &str,
    message_type: &str,
    metadata: Option<&str>,
) -> Result<i64> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO messages (from_session, to_session, to_worktree, to_project, content, message_type, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![from_session, to_session, to_worktree, to_project, content, message_type, metadata, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 指定セッション宛の未読メッセージを取得する
pub fn get_pending_messages(
    conn: &Connection,
    session_id: &str,
    worktree_name: &str,
    project_name: &str,
) -> Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_session, content, message_type, metadata, created_at
         FROM messages
         WHERE read_at IS NULL
           AND (to_session = ?1
                OR to_worktree = ?2
                OR to_project = ?3
                OR (to_session IS NULL AND to_worktree IS NULL AND to_project IS NULL))
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, worktree_name, project_name], |row| {
        Ok(MessageRow {
            id: row.get(0)?,
            from_session: row.get(1)?,
            content: row.get(2)?,
            message_type: row.get(3)?,
            metadata: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// メッセージを既読にする
pub fn mark_messages_read(conn: &Connection, message_ids: &[i64]) -> Result<()> {
    let now = now_unix();
    for id in message_ids {
        conn.execute(
            "UPDATE messages SET read_at = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
    }
    Ok(())
}

/// 全メッセージを取得する（TUI表示用）
pub fn get_all_messages(conn: &Connection, limit: usize) -> Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_session, content, message_type, metadata, created_at
         FROM messages
         ORDER BY id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok(MessageRow {
            id: row.get(0)?,
            from_session: row.get(1)?,
            content: row.get(2)?,
            message_type: row.get(3)?,
            metadata: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    result.reverse(); // 古い順に並べ替え
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub role: String,
    pub worktree_name: String,
    pub project_name: String,
    pub cwd: String,
    pub state: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub from_session: String,
    pub content: String,
    pub message_type: String,
    pub metadata: Option<String>,
    pub created_at: i64,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        init(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn test_init_creates_tables() {
        let conn = test_db();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions', 'messages')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_upsert_and_list_sessions() {
        let conn = test_db();
        upsert_session(&conn, "s1", "frontend", "osaka", "myapp", "/tmp", "idle").unwrap();
        upsert_session(&conn, "s2", "testing", "osaka", "myapp", "/tmp", "working").unwrap();

        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[1].state, "working");
    }

    #[test]
    fn test_update_session_state() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        update_session_state(&conn, "s1", "working").unwrap();

        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions[0].state, "working");
    }

    #[test]
    fn test_update_session_summary() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        update_session_summary(&conn, "s1", "認証フロー実装中").unwrap();

        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions[0].summary.as_deref(), Some("認証フロー実装中"));
    }

    #[test]
    fn test_insert_and_get_messages() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp", "idle").unwrap();
        upsert_session(&conn, "s2", "default", "osaka", "myapp", "/tmp", "idle").unwrap();

        insert_message(&conn, "s1", Some("s2"), None, None, "hello", "message", None).unwrap();

        let msgs = get_pending_messages(&conn, "s2", "osaka", "myapp").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn test_broadcast_message() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp", "idle").unwrap();

        // broadcast: to_session/to_worktree/to_project 全て NULL
        insert_message(&conn, "s1", None, None, None, "broadcast msg", "message", None).unwrap();

        let msgs = get_pending_messages(&conn, "s2", "tokyo", "myapp").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "broadcast msg");
    }

    #[test]
    fn test_worktree_targeted_message() {
        let conn = test_db();
        insert_message(&conn, "s1", None, Some("osaka"), None, "wt msg", "message", None).unwrap();

        let msgs = get_pending_messages(&conn, "s2", "osaka", "myapp").unwrap();
        assert_eq!(msgs.len(), 1);

        // 別の worktree には届かない（to_session が null で to_worktree も不一致）
        let msgs2 = get_pending_messages(&conn, "s3", "tokyo", "myapp").unwrap();
        assert_eq!(msgs2.len(), 0);
    }

    #[test]
    fn test_mark_messages_read() {
        let conn = test_db();
        let id = insert_message(&conn, "s1", Some("s2"), None, None, "msg", "message", None).unwrap();
        mark_messages_read(&conn, &[id]).unwrap();

        let msgs = get_pending_messages(&conn, "s2", "wt", "proj").unwrap();
        assert_eq!(msgs.len(), 0);
    }

    #[test]
    fn test_get_all_messages() {
        let conn = test_db();
        insert_message(&conn, "s1", None, None, None, "msg1", "message", None).unwrap();
        insert_message(&conn, "s2", None, None, None, "msg2", "message", None).unwrap();

        let msgs = get_all_messages(&conn, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        // AUTOINCREMENT id で順序保証（古い順）
        assert!(msgs[0].id < msgs[1].id);
    }

    #[test]
    fn test_dead_sessions_not_listed() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "dead").unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 0);
    }
}

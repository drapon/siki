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
    // 複数プロセスからの同時書き込み時にリトライする（最大5秒）
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;

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

    // マイグレーション: alert カラムが無ければ追加
    let _ = conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN alert INTEGER NOT NULL DEFAULT 0;",
    );
    let _ = conn.execute_batch(
        "ALTER TABLE sessions ADD COLUMN alert_message TEXT;",
    );

    // サマライズ済みセッション追跡テーブル
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS summarized_sessions (
            claude_jsonl_session_id TEXT NOT NULL,
            worktree_cwd TEXT NOT NULL,
            summarized_at INTEGER NOT NULL,
            PRIMARY KEY (claude_jsonl_session_id, worktree_cwd)
        );
        ",
    )?;

    // 会話ログテーブル（セッション完了時にJSONLから保存）
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS conversation_logs (
            session_id TEXT PRIMARY KEY,
            worktree_name TEXT NOT NULL,
            project_name TEXT NOT NULL,
            branch TEXT,
            messages TEXT NOT NULL,
            summary TEXT,
            created_at INTEGER NOT NULL
        );
        ",
    )?;

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
           role = ?2,
           worktree_name = ?3,
           project_name = ?4,
           cwd = ?5,
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

/// セッションのアラート状態を更新する
pub fn update_session_alert(
    conn: &Connection,
    session_id: &str,
    alert: bool,
    message: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET alert = ?1, alert_message = ?2 WHERE session_id = ?3",
        rusqlite::params![alert as i64, message, session_id],
    )?;
    Ok(())
}

/// 指定 worktree のセッションのアラートをクリアする
pub fn clear_alerts_by_worktree(
    conn: &Connection,
    project_name: &str,
    worktree_name: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET alert = 0, alert_message = NULL
         WHERE project_name = ?1 AND worktree_name = ?2 AND alert != 0",
        rusqlite::params![project_name, worktree_name],
    )?;
    Ok(())
}

/// アラートが有効なセッションの一覧を返す（DB→インメモリ同期用）
pub fn get_alerted_sessions(conn: &Connection) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, alert_message FROM sessions WHERE alert != 0 AND state != 'dead'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
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
        "SELECT session_id, role, worktree_name, project_name, cwd, state, summary, alert, alert_message
         FROM sessions WHERE state != 'dead'
         ORDER BY project_name, worktree_name, role",
    )?;
    let rows = stmt.query_map([], |row| {
        let alert_int: i64 = row.get(7)?;
        Ok(SessionRow {
            session_id: row.get(0)?,
            role: row.get(1)?,
            worktree_name: row.get(2)?,
            project_name: row.get(3)?,
            cwd: row.get(4)?,
            state: row.get(5)?,
            summary: row.get(6)?,
            alert: alert_int != 0,
            alert_message: row.get(8)?,
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
           -- 自分が送った fanout（broadcast / worktree / project 宛 = to_session NULL）は
           -- 自分自身には返さない。直接宛 (to_session 指定) はそのまま配信する。
           AND NOT (from_session = ?1 AND to_session IS NULL)
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

/// 単一セッション宛 (`to_session = ?`) のメッセージのみを既読化する。
///
/// broadcast (全 NULL) や worktree/project 宛 fanout は受信者が複数いるため
/// ここでは触らない。一受信者が読んだだけで他受信者に届かなくなる事故を防ぐ。
/// 呼び出し側（SessionStart hook など）が pending を一律 mark すると broadcast の
/// 「全員に届く」セマンティクスが壊れるので、この関数を使うこと。
pub fn mark_messages_read_for_session(
    conn: &Connection,
    session_id: &str,
    message_ids: &[i64],
) -> Result<usize> {
    let now = now_unix();
    let mut marked = 0_usize;
    for id in message_ids {
        let n = conn.execute(
            "UPDATE messages SET read_at = ?1 WHERE id = ?2 AND to_session = ?3 AND read_at IS NULL",
            rusqlite::params![now, id, session_id],
        )?;
        marked += n;
    }
    Ok(marked)
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
    pub alert: bool,
    pub alert_message: Option<String>,
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

// ── conversation_logs ──

#[derive(Debug, Clone)]
pub struct ConversationLogRow {
    pub session_id: String,
    pub worktree_name: String,
    pub project_name: String,
    pub branch: Option<String>,
    pub messages: String,
    pub summary: Option<String>,
    pub created_at: i64,
}

/// 会話ログのうち `messages` 本文を除いた軽量行。件数・サマリ一覧用。
#[derive(Debug, Clone)]
pub struct ConversationLogSummaryRow {
    pub session_id: String,
    pub branch: Option<String>,
    pub summary: Option<String>,
    pub created_at: i64,
}

/// 会話ログを保存する（UPSERT）
pub fn upsert_conversation_log(
    conn: &Connection,
    session_id: &str,
    worktree_name: &str,
    project_name: &str,
    branch: Option<&str>,
    messages_json: &str,
) -> Result<()> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO conversation_logs (session_id, worktree_name, project_name, branch, messages, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id) DO UPDATE SET
           messages = ?5,
           branch = ?4",
        rusqlite::params![session_id, worktree_name, project_name, branch, messages_json, now],
    )?;
    Ok(())
}

/// 会話ログのサマリを更新する
pub fn update_conversation_log_summary(
    conn: &Connection,
    session_id: &str,
    summary: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE conversation_logs SET summary = ?1 WHERE session_id = ?2",
        rusqlite::params![summary, session_id],
    )?;
    Ok(())
}

/// worktree 単位で会話ログを取得する（古い順）
pub fn get_conversation_logs_by_worktree(
    conn: &Connection,
    worktree_name: &str,
    project_name: &str,
) -> Result<Vec<ConversationLogRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, worktree_name, project_name, branch, messages, summary, created_at
         FROM conversation_logs
         WHERE worktree_name = ?1 AND project_name = ?2
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![worktree_name, project_name], |row| {
        Ok(ConversationLogRow {
            session_id: row.get(0)?,
            worktree_name: row.get(1)?,
            project_name: row.get(2)?,
            branch: row.get(3)?,
            messages: row.get(4)?,
            summary: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// 会話ログのサマリ情報を worktree 単位で取得する（古い順）。
///
/// `get_conversation_logs_by_worktree` と異なり、肥大化しがちな `messages` 本文を
/// SELECT しない軽量版。件数表示やサマリ一覧のように本文が不要な経路で使う
/// （`messages` を載せると 100KB 級の行をメモリに展開してしまうため）。
pub fn get_conversation_log_summaries_by_worktree(
    conn: &Connection,
    worktree_name: &str,
    project_name: &str,
) -> Result<Vec<ConversationLogSummaryRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, branch, summary, created_at
         FROM conversation_logs
         WHERE worktree_name = ?1 AND project_name = ?2
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![worktree_name, project_name], |row| {
        Ok(ConversationLogSummaryRow {
            session_id: row.get(0)?,
            branch: row.get(1)?,
            summary: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// session_id で会話ログの messages を取得する
pub fn get_conversation_log_messages(conn: &Connection, session_id: &str) -> Result<String> {
    let messages: String = conn.query_row(
        "SELECT messages FROM conversation_logs WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?;
    Ok(messages)
}

/// 保存済みセッションIDの一覧を取得する（未保存JSONL検出用）
pub fn get_saved_conversation_session_ids(
    conn: &Connection,
    worktree_name: &str,
    project_name: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT session_id FROM conversation_logs WHERE worktree_name = ?1 AND project_name = ?2",
    )?;
    let ids = stmt
        .query_map(rusqlite::params![worktree_name, project_name], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// サマライズ済みセッションIDを記録する
pub fn mark_sessions_summarized(
    conn: &Connection,
    session_ids: &[String],
    worktree_cwd: &str,
) -> Result<()> {
    let now = now_unix();
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO summarized_sessions (claude_jsonl_session_id, worktree_cwd, summarized_at) VALUES (?1, ?2, ?3)",
    )?;
    for id in session_ids {
        stmt.execute(rusqlite::params![id, worktree_cwd, now])?;
    }
    Ok(())
}

/// 指定 worktree のサマライズ済みセッションIDを取得する
pub fn get_summarized_session_ids(
    conn: &Connection,
    worktree_cwd: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT claude_jsonl_session_id FROM summarized_sessions WHERE worktree_cwd = ?1",
    )?;
    let ids = stmt
        .query_map(rusqlite::params![worktree_cwd], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(ids)
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
    fn test_upsert_session_updates_metadata_on_conflict() {
        let conn = test_db();
        // 初回: unknown worktree で登録（レースコンディションで空 cwd 登録されたケース）
        upsert_session(&conn, "s1", "default", "unknown", "unknown", "", "idle").unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions[0].worktree_name, "unknown");
        assert_eq!(sessions[0].project_name, "unknown");

        // 2回目: 正しい情報で Register が到着
        upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp/osaka", "idle").unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].worktree_name, "osaka");
        assert_eq!(sessions[0].project_name, "myapp");
        assert_eq!(sessions[0].cwd, "/tmp/osaka");
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
    fn test_upsert_conversation_log() {
        let conn = test_db();
        let msgs = r#"[{"role":"user","content":"hello"}]"#;
        upsert_conversation_log(&conn, "sess1", "osaka", "myapp", Some("main"), msgs).unwrap();

        let logs = get_conversation_logs_by_worktree(&conn, "osaka", "myapp").unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].session_id, "sess1");
        assert_eq!(logs[0].branch.as_deref(), Some("main"));
        assert_eq!(logs[0].messages, msgs);
        assert!(logs[0].summary.is_none());
    }

    #[test]
    fn test_upsert_conversation_log_updates_on_conflict() {
        let conn = test_db();
        let msgs1 = r#"[{"role":"user","content":"v1"}]"#;
        let msgs2 = r#"[{"role":"user","content":"v1"},{"role":"assistant","content":"v2"}]"#;
        upsert_conversation_log(&conn, "sess1", "osaka", "myapp", Some("main"), msgs1).unwrap();
        upsert_conversation_log(&conn, "sess1", "osaka", "myapp", Some("main"), msgs2).unwrap();

        let logs = get_conversation_logs_by_worktree(&conn, "osaka", "myapp").unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].messages, msgs2);
    }

    #[test]
    fn test_update_conversation_log_summary() {
        let conn = test_db();
        upsert_conversation_log(&conn, "sess1", "osaka", "myapp", None, "[]").unwrap();
        update_conversation_log_summary(&conn, "sess1", "認証フロー実装").unwrap();

        let logs = get_conversation_logs_by_worktree(&conn, "osaka", "myapp").unwrap();
        assert_eq!(logs[0].summary.as_deref(), Some("認証フロー実装"));
    }

    #[test]
    fn test_get_saved_conversation_session_ids() {
        let conn = test_db();
        upsert_conversation_log(&conn, "s1", "osaka", "myapp", None, "[]").unwrap();
        upsert_conversation_log(&conn, "s2", "osaka", "myapp", None, "[]").unwrap();
        upsert_conversation_log(&conn, "s3", "tokyo", "myapp", None, "[]").unwrap();

        let ids = get_saved_conversation_session_ids(&conn, "osaka", "myapp").unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_dead_sessions_not_listed() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "dead").unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 0);
    }

    #[test]
    fn test_update_session_alert() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();

        // アラート設定
        update_session_alert(&conn, "s1", true, Some("CI failed")).unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert!(sessions[0].alert);
        assert_eq!(sessions[0].alert_message.as_deref(), Some("CI failed"));

        // アラート解除
        update_session_alert(&conn, "s1", false, None).unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert!(!sessions[0].alert);
        assert!(sessions[0].alert_message.is_none());
    }

    #[test]
    fn test_session_alert_default() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        let sessions = list_sessions(&conn).unwrap();
        assert!(!sessions[0].alert);
        assert!(sessions[0].alert_message.is_none());
    }

    #[test]
    fn test_clear_alerts_by_worktree() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt1", "proj", "/tmp", "idle").unwrap();
        upsert_session(&conn, "s2", "default", "wt1", "proj", "/tmp", "idle").unwrap();
        upsert_session(&conn, "s3", "default", "wt2", "proj", "/tmp", "idle").unwrap();
        update_session_alert(&conn, "s1", true, Some("CI failed")).unwrap();
        update_session_alert(&conn, "s3", true, Some("test failed")).unwrap();

        clear_alerts_by_worktree(&conn, "proj", "wt1").unwrap();

        let sessions = list_sessions(&conn).unwrap();
        let s1 = sessions.iter().find(|s| s.session_id == "s1").unwrap();
        let s3 = sessions.iter().find(|s| s.session_id == "s3").unwrap();
        assert!(!s1.alert);
        assert!(s3.alert); // 別 worktree は影響なし
    }

    #[test]
    fn test_get_alerted_sessions() {
        let conn = test_db();
        upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        upsert_session(&conn, "s2", "default", "wt", "proj", "/tmp", "idle").unwrap();
        update_session_alert(&conn, "s1", true, Some("CI failed")).unwrap();

        let alerted = get_alerted_sessions(&conn).unwrap();
        assert_eq!(alerted.len(), 1);
        assert_eq!(alerted[0].0, "s1");
        assert_eq!(alerted[0].1.as_deref(), Some("CI failed"));
    }
}

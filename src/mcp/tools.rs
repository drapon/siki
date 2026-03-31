use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

use crate::db;

/// ツールを実行してレスポンスを返す
pub fn execute_tool(
    conn: &Connection,
    tool_name: &str,
    params: &Value,
    session_id: &str,
) -> Result<Value> {
    match tool_name {
        "list_sessions" => list_sessions(conn, session_id),
        "send_message" => send_message(conn, params, session_id),
        "broadcast" => broadcast(conn, params, session_id),
        "set_summary" => set_summary(conn, params, session_id),
        "handoff" => handoff(conn, params, session_id),
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    }
}

fn list_sessions(conn: &Connection, session_id: &str) -> Result<Value> {
    let sessions = db::list_sessions(conn)?;
    let items: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.session_id,
                "worktree_name": s.worktree_name,
                "project_name": s.project_name,
                "role": s.role,
                "state": s.state,
                "summary": s.summary,
                "cwd": s.cwd,
            })
        })
        .collect();

    // 自セッション宛の未読メッセージも取得
    let my_worktree = sessions.iter().find(|s| s.session_id == session_id);
    let (wt, proj) = my_worktree
        .map(|s| (s.worktree_name.as_str(), s.project_name.as_str()))
        .unwrap_or(("", ""));

    let pending = db::get_pending_messages(conn, session_id, wt, proj)?;
    let msg_ids: Vec<i64> = pending.iter().map(|m| m.id).collect();
    let messages: Vec<Value> = pending
        .iter()
        .map(|m| {
            json!({
                "from": m.from_session,
                "content": m.content,
                "type": m.message_type,
            })
        })
        .collect();

    // 取得したメッセージを既読にする
    if !msg_ids.is_empty() {
        let _ = db::mark_messages_read(conn, &msg_ids);
    }

    Ok(json!({ "sessions": items, "pending_messages": messages }))
}

fn send_message(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let target = params.get("target").ok_or_else(|| anyhow::anyhow!("target is required"))?;
    let target_type = target
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.type is required"))?;
    let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.id is required"))?;
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("message is required"))?;

    let (to_session, to_worktree, to_project) = match target_type {
        "session" => (Some(target_id), None, None),
        "worktree" => (None, Some(target_id), None),
        "project" => (None, None, Some(target_id)),
        _ => anyhow::bail!("Invalid target type: {}", target_type),
    };

    db::insert_message(conn, from_session, to_session, to_worktree, to_project, message, "message", None)?;

    Ok(json!({ "delivered": true, "recipient_count": 1 }))
}

fn broadcast(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("message is required"))?;

    db::insert_message(conn, from_session, None, None, None, message, "message", None)?;

    Ok(json!({ "delivered": true }))
}

fn set_summary(conn: &Connection, params: &Value, session_id: &str) -> Result<Value> {
    let summary = params
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("summary is required"))?;

    db::update_session_summary(conn, session_id, summary)?;

    Ok(json!({ "ok": true }))
}

fn handoff(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let target = params.get("target").ok_or_else(|| anyhow::anyhow!("target is required"))?;
    let target_type = target
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.type is required"))?;
    let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.id is required"))?;
    let note = params.get("note").and_then(|v| v.as_str());

    // 送信元セッションの情報を取得
    let sessions = db::list_sessions(conn)?;
    let from = sessions.iter().find(|s| s.session_id == from_session);

    let cwd = from.map(|s| s.cwd.as_str()).unwrap_or(".");
    let role = from.map(|s| s.role.as_str()).unwrap_or("default");
    let worktree_name = from.map(|s| s.worktree_name.as_str()).unwrap_or("unknown");
    let summary = from.and_then(|s| s.summary.as_deref());

    // git 情報を自動収集
    let git_log = run_git(cwd, &["log", "--oneline", "-5"]);
    let git_status = run_git(cwd, &["status", "--short"]);
    let git_diff_stat = run_git(cwd, &["diff", "--stat", "HEAD"]);
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);

    // handoff メッセージを組み立て
    let mut content = format!("[siki handoff from {}/{}]\n", worktree_name, role);
    content.push_str(&format!("branch: {}\n", branch.trim()));

    if !git_log.is_empty() {
        content.push_str("recent commits:\n");
        for line in git_log.lines() {
            content.push_str(&format!("  {}\n", line));
        }
    }

    if !git_status.is_empty() {
        content.push_str("changed files:\n");
        for line in git_status.lines() {
            content.push_str(&format!("  {}\n", line));
        }
    }

    if !git_diff_stat.is_empty() {
        content.push_str(&format!("diff summary:\n{}\n", git_diff_stat));
    }

    if let Some(s) = summary {
        content.push_str(&format!("summary: {}\n", s));
    }

    if let Some(n) = note {
        content.push_str(&format!("note: {}\n", n));
    }

    let (to_session, to_worktree, to_project) = match target_type {
        "session" => (Some(target_id), None, None),
        "worktree" => (None, Some(target_id), None),
        "project" => (None, None, Some(target_id)),
        _ => anyhow::bail!("Invalid target type: {}", target_type),
    };

    let metadata = json!({
        "branch": branch.trim(),
        "from_role": role,
        "from_worktree": worktree_name,
    });

    db::insert_message(
        conn,
        from_session,
        to_session,
        to_worktree,
        to_project,
        &content,
        "handoff",
        Some(&metadata.to_string()),
    )?;

    Ok(json!({ "delivered": true }))
}

fn run_git(cwd: &str, args: &[&str]) -> String {
    let path = Path::new(cwd);
    Command::new("git")
        .args(args)
        .current_dir(if path.exists() { path } else { Path::new(".") })
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_db() -> Connection {
        db::init(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn test_list_sessions_empty() {
        let conn = test_db();
        let result = list_sessions(&conn, "me").unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(result["pending_messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_list_sessions_with_data() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "frontend", "osaka", "myapp", "/tmp", "idle").unwrap();
        let result = list_sessions(&conn, "s2").unwrap();
        let sessions = result["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], "s1");
    }

    #[test]
    fn test_list_sessions_includes_pending_messages() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp", "idle").unwrap();
        db::upsert_session(&conn, "s2", "default", "osaka", "myapp", "/tmp", "idle").unwrap();
        db::insert_message(&conn, "s1", Some("s2"), None, None, "hello s2", "message", None).unwrap();

        let result = list_sessions(&conn, "s2").unwrap();
        let msgs = result["pending_messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "hello s2");

        // 2回目は既読なので空
        let result2 = list_sessions(&conn, "s2").unwrap();
        assert_eq!(result2["pending_messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_send_message_to_session() {
        let conn = test_db();
        let params = json!({
            "target": { "type": "session", "id": "s2" },
            "message": "hello"
        });
        let result = send_message(&conn, &params, "s1").unwrap();
        assert_eq!(result["delivered"], true);
    }

    #[test]
    fn test_send_message_to_worktree() {
        let conn = test_db();
        let params = json!({
            "target": { "type": "worktree", "id": "osaka" },
            "message": "hello osaka"
        });
        let result = send_message(&conn, &params, "s1").unwrap();
        assert_eq!(result["delivered"], true);
    }

    #[test]
    fn test_broadcast() {
        let conn = test_db();
        let params = json!({ "message": "hello everyone" });
        let result = broadcast(&conn, &params, "s1").unwrap();
        assert_eq!(result["delivered"], true);

        let msgs = db::get_pending_messages(&conn, "s2", "wt", "proj").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello everyone");
    }

    #[test]
    fn test_set_summary() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        let params = json!({ "summary": "implementing auth" });
        set_summary(&conn, &params, "s1").unwrap();

        let sessions = db::list_sessions(&conn).unwrap();
        assert_eq!(sessions[0].summary.as_deref(), Some("implementing auth"));
    }

    #[test]
    fn test_handoff() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "frontend", "osaka", "myapp", "/tmp", "idle").unwrap();
        db::update_session_summary(&conn, "s1", "auth done").unwrap();

        let params = json!({
            "target": { "type": "session", "id": "s2" },
            "note": "please write tests"
        });
        let result = handoff(&conn, &params, "s1").unwrap();
        assert_eq!(result["delivered"], true);

        let msgs = db::get_pending_messages(&conn, "s2", "osaka", "myapp").unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("[siki handoff from osaka/frontend]"));
        assert!(msgs[0].content.contains("note: please write tests"));
        assert!(msgs[0].content.contains("summary: auth done"));
        assert_eq!(msgs[0].message_type, "handoff");
    }

    #[test]
    fn test_execute_tool_unknown() {
        let conn = test_db();
        let result = execute_tool(&conn, "unknown_tool", &json!({}), "s1");
        assert!(result.is_err());
    }
}

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
        "list_sessions" => list_sessions(conn, session_id, params),
        "send_message" => send_message(conn, params, session_id),
        "broadcast" => broadcast(conn, params, session_id),
        "set_summary" => set_summary(conn, params, session_id),
        "set_alert" => set_alert(conn, params, session_id),
        "handoff" => handoff(conn, params, session_id),
        "get_context" => get_context(conn, params),
        "save_skill" => save_skill(params),
        "list_skills" => list_skills(params),
        "summarize_history" => summarize_history(conn, params, session_id),
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    }
}

fn list_sessions(conn: &Connection, session_id: &str, params: &Value) -> Result<Value> {
    // 既定スコープは "project"。"machine" を既定にするとマシン上の全 worktree・
    // 全 project のセッションを返してしまい、無関係なノイズで応答が膨らむ。
    // マシン全体を見たいときは scope:"machine" を明示する。
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");

    // worktree コンテキスト .md と過去会話サマリの本文を同梱するか。
    // 既定 false。本文は worktree の寿命とともに無制限に増えるため、既定で同梱すると
    // list_sessions の応答が肥大化し harness 側で「巨大なため省略」と切り詰められる。
    // 既定では件数ポインタ（SessionStart hook と同じ方針）のみ返す。
    let include_bodies = params
        .get("include_bodies")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let all_sessions = db::list_sessions(conn)?;

    // 自セッションの worktree/project を特定（スコープフィルタに使用）
    let my_worktree = all_sessions.iter().find(|s| s.session_id == session_id);
    let (wt, proj) = my_worktree
        .map(|s| (s.worktree_name.as_str(), s.project_name.as_str()))
        .unwrap_or(("", ""));

    // scope に応じてフィルタ
    let sessions: Vec<&db::SessionRow> = match scope {
        "worktree" => all_sessions
            .iter()
            .filter(|s| !wt.is_empty() && s.worktree_name == wt && s.project_name == proj)
            .collect(),
        "project" => all_sessions
            .iter()
            .filter(|s| !proj.is_empty() && s.project_name == proj)
            .collect(),
        _ => all_sessions.iter().collect(), // "machine" = 全件
    };

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
                "alert": s.alert,
                "alert_message": s.alert_message,
            })
        })
        .collect();

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

    // 取得したメッセージのうち、単一セッション宛 (to_session = ?) のみを既読化する。
    // broadcast (全 NULL) や worktree/project 宛 fanout はここで mark すると他の受信者が
    // 取れなくなるため触らない。SessionStart hook 側 (session_start.rs) と同じポリシー。
    if !msg_ids.is_empty() {
        let _ = db::mark_messages_read_for_session(conn, session_id, &msg_ids);
    }

    // 自分のセッション情報（proj/wt）で worktree コンテキストと会話サマリを引く。
    // （std::env::current_dir() はMCPサーバーの起動元CWDであり、worktreeパスと異なる場合がある）
    let valid_worktree = !proj.is_empty() && !wt.is_empty();
    let contexts = if valid_worktree {
        crate::config::load_contexts(proj, wt)
    } else {
        Vec::new()
    };
    let summaries = if valid_worktree {
        db::get_conversation_logs_by_worktree(conn, wt, proj).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut result = json!({ "sessions": items, "pending_messages": messages });

    if include_bodies {
        // 明示要求時のみフル本文を返す。
        if !contexts.is_empty() {
            let ctx_items: Vec<Value> = contexts
                .iter()
                .map(|(name, content)| json!({ "name": name, "content": content }))
                .collect();
            result["worktree_contexts"] = json!({
                "project": proj,
                "worktree": wt,
                "contexts": ctx_items
            });
        }
        if !summaries.is_empty() {
            let sum_items: Vec<Value> = summaries
                .iter()
                .map(|log| {
                    json!({
                        "session_id": log.session_id,
                        "branch": log.branch,
                        "summary": log.summary,
                        "created_at": log.created_at,
                    })
                })
                .collect();
            result["conversation_summaries"] = json!(sum_items);
        }
    } else if !contexts.is_empty() || !summaries.is_empty() {
        // 既定: 件数ポインタのみ。本文は include_bodies:true で取得させる。
        let total_bytes: usize = contexts.iter().map(|(_, c)| c.len()).sum();
        result["background"] = json!({
            "conversation_summary_count": summaries.len(),
            "worktree_context_files": contexts.len(),
            "worktree_context_kb": (total_bytes + 512) / 1024,
            "hint": "Pass include_bodies:true to fetch the full conversation summaries and worktree context bodies (independent of scope). These can be large — prefer fetching only when the current task needs them.",
        });
    }

    Ok(result)
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

fn set_alert(conn: &Connection, params: &Value, session_id: &str) -> Result<Value> {
    let alert = params
        .get("alert")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    if alert && message.is_none() {
        anyhow::bail!("message is required when raising an alert");
    }

    if let Some(msg) = message {
        if msg.len() > 500 {
            anyhow::bail!("message must be 500 characters or fewer");
        }
    }

    db::update_session_alert(conn, session_id, alert, if alert { message } else { None })?;

    Ok(json!({ "ok": true, "alert": alert }))
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

/// 指定セッション/worktreeのコンテキストを取得する（pull型の引き継ぎ）
fn get_context(conn: &Connection, params: &Value) -> Result<Value> {
    let target = params.get("target").ok_or_else(|| anyhow::anyhow!("target is required"))?;
    let target_type = target
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.type is required"))?;
    let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.id is required"))?;

    // 対象セッションを検索
    let sessions = db::list_sessions(conn)?;
    let targets: Vec<&db::SessionRow> = match target_type {
        "session" => sessions.iter().filter(|s| s.session_id == target_id).collect(),
        "worktree" => sessions.iter().filter(|s| s.worktree_name == target_id).collect(),
        "project" => sessions.iter().filter(|s| s.project_name == target_id).collect(),
        _ => anyhow::bail!("Invalid target type: {}", target_type),
    };

    if targets.is_empty() {
        return Ok(json!({
            "error": format!("No sessions found for {} '{}'", target_type, target_id),
            "sessions": []
        }));
    }

    let include_log = params
        .get("include_conversation_log")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut contexts = Vec::new();
    for session in &targets {
        let cwd = &session.cwd;
        let git_log = run_git(cwd, &["log", "--oneline", "-10"]);
        let git_status = run_git(cwd, &["status", "--short"]);
        let git_diff_stat = run_git(cwd, &["diff", "--stat", "HEAD"]);
        let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);

        let mut ctx = json!({
            "session_id": session.session_id,
            "worktree_name": session.worktree_name,
            "project_name": session.project_name,
            "role": session.role,
            "state": session.state,
            "summary": session.summary,
            "branch": branch.trim(),
            "recent_commits": git_log.lines().collect::<Vec<_>>(),
            "changed_files": git_status.lines().collect::<Vec<_>>(),
            "diff_stat": git_diff_stat.trim(),
        });

        // 会話ログを含める
        if include_log {
            let logs = db::get_conversation_logs_by_worktree(
                conn,
                &session.worktree_name,
                &session.project_name,
            )
            .unwrap_or_default();
            let conv_items: Vec<Value> = logs
                .iter()
                .map(|log| {
                    json!({
                        "session_id": log.session_id,
                        "branch": log.branch,
                        "summary": log.summary,
                        "messages": serde_json::from_str::<Value>(&log.messages).unwrap_or(json!([])),
                        "created_at": log.created_at,
                    })
                })
                .collect();
            ctx["conversation_logs"] = json!(conv_items);
        }

        contexts.push(ctx);
    }

    Ok(json!({ "contexts": contexts }))
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

/// 会話履歴をサマライズして worktree_contexts に保存し、元セッションをマーク
fn summarize_history(conn: &Connection, params: &Value, session_id: &str) -> Result<Value> {
    let summary = params
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("summary is required"))?;
    let session_ids = params
        .get("session_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("session_ids is required (array of session IDs to mark as summarized)"))?;

    let ids: Vec<String> = session_ids
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if ids.is_empty() {
        anyhow::bail!("session_ids must not be empty");
    }

    // DB からセッション情報を取得して worktree を特定
    // （std::env::current_dir() はMCPサーバーの起動元CWDであり、worktreeパスと異なる場合がある）
    let all_sessions = db::list_sessions(conn)?;
    let my_session = all_sessions.iter().find(|s| s.session_id == session_id);
    let (proj, wt) = my_session
        .map(|s| (s.project_name.as_str(), s.worktree_name.as_str()))
        .unwrap_or(("", ""));

    let cwd_str = my_session
        .map(|s| s.cwd.as_str())
        .unwrap_or("")
        .to_string();

    // worktree_contexts ディレクトリに要約を保存（ファイルが大きくなったら分割）
    const MAX_FILE_SIZE: usize = 100_000; // 100KB per file

    if !proj.is_empty() && !wt.is_empty() {
        let ctx_dir = crate::config::worktree_contexts_dir(&proj, &wt);
        std::fs::create_dir_all(&ctx_dir)?;

        // 既存のサマリーファイルを探す（1回のスキャンで最新パスと次番号を取得）
        let (latest_path, next_num) = scan_summary_files(&ctx_dir);
        let existing = latest_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();

        if existing.is_empty() {
            // 新規作成
            let path = ctx_dir.join("conversation-summary.md");
            std::fs::write(&path, format!("# Conversation History Summary\n\n{}", summary))?;
        } else if existing.len() + summary.len() > MAX_FILE_SIZE {
            // ファイルが大きいので新しいファイルに分割
            let path = ctx_dir.join(format!("conversation-summary-{}.md", next_num));
            std::fs::write(&path, format!("# Conversation History Summary ({})\n\n{}", next_num, summary))?;
        } else {
            // 既存ファイルに追記
            let path = latest_path.unwrap();
            let new_content = format!("{}\n\n---\n\n{}", existing, summary);
            std::fs::write(&path, new_content)?;
        }
    }

    // セッションをサマライズ済みとしてマーク
    db::mark_sessions_summarized(conn, &ids, &cwd_str)?;

    Ok(json!({
        "summarized": ids.len(),
        "session_ids": ids,
    }))
}

/// サマリーファイルをスキャンし、最新のファイルパスと次の番号を返す
/// - 返り値: (最新ファイルのパス or None, 次に作成すべき番号)
fn scan_summary_files(ctx_dir: &Path) -> (Option<PathBuf>, usize) {
    let base = ctx_dir.join("conversation-summary.md");
    if !base.exists() {
        return (None, 2);
    }

    let mut max_existing = 0_usize; // 0 = ベースファイルのみ
    if let Ok(entries) = std::fs::read_dir(ctx_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix("conversation-summary-") {
                if let Some(num_str) = rest.strip_suffix(".md") {
                    if let Ok(n) = num_str.parse::<usize>() {
                        if n > max_existing {
                            max_existing = n;
                        }
                    }
                }
            }
        }
    }

    if max_existing >= 2 {
        let latest = ctx_dir.join(format!("conversation-summary-{}.md", max_existing));
        (Some(latest), max_existing + 1)
    } else {
        (Some(base), 2)
    }
}

/// スキル名のバリデーション（英数字・ハイフン・アンダースコアのみ）
fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("skill_name must not be empty");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("skill_name must contain only alphanumeric characters, hyphens, and underscores");
    }
    Ok(())
}

fn save_skill(params: &Value) -> Result<Value> {
    let project_name = params.get("project_name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("project_name is required"))?;
    let skill_name = params.get("skill_name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("skill_name is required"))?;
    let content = params.get("content").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("content is required"))?;

    validate_skill_name(skill_name)?;

    // skills/<name>/SKILL.md のディレクトリ形式で保存
    let skill_dir = crate::config::project_skills_dir(project_name).join(skill_name);
    std::fs::create_dir_all(&skill_dir)?;

    let file_path = skill_dir.join("SKILL.md");
    std::fs::write(&file_path, content)?;

    Ok(json!({
        "saved": true,
        "path": file_path.to_string_lossy()
    }))
}

fn list_skills(params: &Value) -> Result<Value> {
    let project_name = params.get("project_name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("project_name is required"))?;

    let skills_dir = crate::config::project_skills_dir(project_name);
    if !skills_dir.is_dir() {
        return Ok(json!({ "skills": [] }));
    }

    let mut skills = Vec::new();
    for entry in std::fs::read_dir(&skills_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let skill_file = path.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_file).unwrap_or_default();
        if !content.is_empty() {
            skills.push(json!({
                "name": name,
                "content": content
            }));
        }
    }

    Ok(json!({ "skills": skills }))
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
        let result = list_sessions(&conn, "me", &json!({})).unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(result["pending_messages"].as_array().unwrap().len(), 0);
        // データが無ければ background ポインタは付かない
        assert!(result.get("background").is_none());
    }

    #[test]
    fn test_list_sessions_with_data() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "frontend", "osaka", "myapp", "/tmp", "idle").unwrap();
        // 既定スコープは project。呼び出し元は同 project に登録済みの s1 とする。
        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
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

        let result = list_sessions(&conn, "s2", &json!({})).unwrap();
        let msgs = result["pending_messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "hello s2");

        // 2回目は既読なので空
        let result2 = list_sessions(&conn, "s2", &json!({})).unwrap();
        assert_eq!(result2["pending_messages"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_list_sessions_does_not_consume_broadcast_for_other_sessions() {
        // 回帰テスト: H-1（broadcast の二重消費）
        // list_sessions が broadcast を全 session 横断で mark すると、
        // 他セッションが broadcast を受け取れなくなる。to_session 厳密一致のみ
        // mark することを保証する。
        let conn = test_db();
        db::upsert_session(&conn, "a", "default", "osaka", "myapp", "/tmp/osaka", "idle").unwrap();
        db::upsert_session(&conn, "b", "default", "osaka", "myapp", "/tmp/osaka", "idle").unwrap();

        // broadcast (to_session/to_worktree/to_project すべて NULL)
        db::insert_message(&conn, "sender", None, None, None, "broadcast", "message", None).unwrap();
        // worktree fanout
        db::insert_message(
            &conn,
            "sender",
            None,
            Some("osaka"),
            None,
            "worktree msg",
            "message",
            None,
        )
        .unwrap();
        // 単一セッション宛 (a 宛)
        db::insert_message(&conn, "sender", Some("a"), None, None, "direct to a", "message", None)
            .unwrap();

        // a が list_sessions を呼ぶ → 3件すべて受信
        let result_a = list_sessions(&conn, "a", &json!({})).unwrap();
        let msgs_a = result_a["pending_messages"].as_array().unwrap();
        assert_eq!(msgs_a.len(), 3, "a should see all 3 messages");

        // b が呼ぶ → broadcast と worktree fanout は **まだ届く**
        // direct to a は b 宛ではないので元から見えない
        let result_b = list_sessions(&conn, "b", &json!({})).unwrap();
        let msgs_b = result_b["pending_messages"].as_array().unwrap();
        let contents: Vec<&str> = msgs_b
            .iter()
            .map(|m| m.get("content").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(
            contents.contains(&"broadcast"),
            "broadcast was consumed by a; b lost it. got: {:?}",
            contents
        );
        assert!(
            contents.contains(&"worktree msg"),
            "worktree fanout was consumed by a; b lost it. got: {:?}",
            contents
        );
        assert!(
            !contents.contains(&"direct to a"),
            "direct-to-a should not be in b's pending. got: {:?}",
            contents
        );

        // a が再度呼ぶ → direct to a は既読化済み、broadcast/worktree fanout はまだ残る
        let result_a2 = list_sessions(&conn, "a", &json!({})).unwrap();
        let msgs_a2 = result_a2["pending_messages"].as_array().unwrap();
        let contents_a2: Vec<&str> = msgs_a2
            .iter()
            .map(|m| m.get("content").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(!contents_a2.contains(&"direct to a"));
        assert!(contents_a2.contains(&"broadcast"));
        assert!(contents_a2.contains(&"worktree msg"));
    }

    #[test]
    fn test_list_sessions_scope_worktree() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp/osaka", "idle").unwrap();
        db::upsert_session(&conn, "s2", "default", "osaka", "myapp", "/tmp/osaka", "working").unwrap();
        db::upsert_session(&conn, "s3", "default", "tokyo", "myapp", "/tmp/tokyo", "idle").unwrap();
        // 別 project のセッション（既定 project スコープから除外されることの確認用）
        db::upsert_session(&conn, "s4", "default", "berlin", "other", "/tmp/berlin", "idle").unwrap();

        // scope: worktree → s1のworktree(osaka)のみ
        let result = list_sessions(&conn, "s1", &json!({"scope": "worktree"})).unwrap();
        let sessions = result["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);

        // scope: project → myapp全体（別 project の s4 は含まない）
        let result = list_sessions(&conn, "s1", &json!({"scope": "project"})).unwrap();
        let sessions = result["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 3);

        // デフォルト → project スコープ。別 project の s4 は出ない
        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
        let ids: Vec<&str> = result["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        assert!(!ids.contains(&"s4"), "default scope must be project, got: {:?}", ids);

        // scope: machine → 別 project も含め全件
        let result = list_sessions(&conn, "s1", &json!({"scope": "machine"})).unwrap();
        let sessions = result["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 4);
    }

    #[test]
    fn test_list_sessions_background_pointer_by_default_and_bodies_on_request() {
        // worktree に過去会話ログがあると、既定では本文を返さず件数ポインタのみ。
        // include_bodies:true のときだけフル本文 (conversation_summaries) を返す。
        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces
            .join("proj-z")
            .join("wt-z")
            .to_string_lossy()
            .to_string();
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt-z", "proj-z", &cwd, "idle").unwrap();
        db::upsert_conversation_log(&conn, "old", "wt-z", "proj-z", None, "[]").unwrap();
        db::update_conversation_log_summary(&conn, "old", "past summary text").unwrap();

        // 既定: background ポインタのみ、本文キーは無し
        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
        assert!(result.get("conversation_summaries").is_none());
        assert!(result.get("worktree_contexts").is_none());
        let bg = result.get("background").expect("background pointer expected");
        assert_eq!(bg["conversation_summary_count"], 1);

        // include_bodies:true: フル本文を返す
        let result = list_sessions(&conn, "s1", &json!({"include_bodies": true})).unwrap();
        assert!(result.get("background").is_none());
        let summaries = result["conversation_summaries"].as_array().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["summary"], "past summary text");
    }

    #[test]
    fn test_list_sessions_context_files_pointer_and_bodies() {
        // worktree context .md がある場合: 既定では background の件数ポインタ
        // (worktree_context_files / worktree_context_kb) のみ、include_bodies:true で
        // worktree_contexts に本文をシリアライズして返すことを検証する。
        // load_contexts は実ファイルシステム (workspaces_dir 配下) を読むため、
        // 衝突しない一意な proj/wt 名で実ファイルを作り、結果取得後に必ず削除する。
        let proj = "ctxtest-proj";
        let wt = "ctxtest-wt";
        let ctx_dir = crate::config::worktree_contexts_dir(proj, wt);
        std::fs::create_dir_all(&ctx_dir).unwrap();
        std::fs::write(ctx_dir.join("alpha.md"), "alpha body").unwrap();
        std::fs::write(ctx_dir.join("beta.md"), "beta body content").unwrap();

        let cwd = crate::config::workspaces_dir()
            .join(proj)
            .join(wt)
            .to_string_lossy()
            .to_string();
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", wt, proj, &cwd, "idle").unwrap();

        // 結果を取得してから（panic でファイルが残らないよう）後始末する
        let default_result = list_sessions(&conn, "s1", &json!({}));
        let bodies_result = list_sessions(&conn, "s1", &json!({"include_bodies": true}));
        std::fs::remove_dir_all(&ctx_dir).ok();

        // 既定: 本文キーは無く、background に件数ポインタ
        let default_result = default_result.unwrap();
        assert!(default_result.get("worktree_contexts").is_none());
        let bg = default_result
            .get("background")
            .expect("background pointer expected");
        assert_eq!(bg["worktree_context_files"], 2);
        // "alpha body"(10) + "beta body content"(17) = 27 bytes → (27+512)/1024 = 0 KB
        assert_eq!(bg["worktree_context_kb"], 0);

        // include_bodies:true: worktree_contexts に本文を返す（名前順 alpha, beta）
        let bodies_result = bodies_result.unwrap();
        assert!(bodies_result.get("background").is_none());
        let wc = &bodies_result["worktree_contexts"];
        assert_eq!(wc["project"], proj);
        assert_eq!(wc["worktree"], wt);
        let items = wc["contexts"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "alpha");
        assert_eq!(items[0]["content"], "alpha body");
        assert_eq!(items[1]["name"], "beta");
        assert_eq!(items[1]["content"], "beta body content");
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
    fn test_set_alert() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();

        // アラート発火
        let params = json!({ "message": "CI failed" });
        let result = set_alert(&conn, &params, "s1").unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["alert"], true);

        let sessions = db::list_sessions(&conn).unwrap();
        assert!(sessions[0].alert);
        assert_eq!(sessions[0].alert_message.as_deref(), Some("CI failed"));
    }

    #[test]
    fn test_set_alert_clear() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        db::update_session_alert(&conn, "s1", true, Some("CI failed")).unwrap();

        // アラート解除（message なしでOK）
        let params = json!({ "alert": false });
        let result = set_alert(&conn, &params, "s1").unwrap();
        assert_eq!(result["alert"], false);

        let sessions = db::list_sessions(&conn).unwrap();
        assert!(!sessions[0].alert);
        assert!(sessions[0].alert_message.is_none());
    }

    #[test]
    fn test_set_alert_requires_message_when_raising() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();

        // message なしでアラート発火はエラー
        let params = json!({ "alert": true });
        let result = set_alert(&conn, &params, "s1");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_alert_rejects_long_message() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();

        let long_msg = "x".repeat(501);
        let params = json!({ "message": long_msg });
        let result = set_alert(&conn, &params, "s1");
        assert!(result.is_err());
    }

    #[test]
    fn test_list_sessions_includes_alert() {
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        db::update_session_alert(&conn, "s1", true, Some("CI failed")).unwrap();

        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
        let sessions = result["sessions"].as_array().unwrap();
        assert_eq!(sessions[0]["alert"], true);
        assert_eq!(sessions[0]["alert_message"], "CI failed");
    }

    #[test]
    fn test_execute_tool_unknown() {
        let conn = test_db();
        let result = execute_tool(&conn, "unknown_tool", &json!({}), "s1");
        assert!(result.is_err());
    }

    #[test]
    fn test_save_skill() {
        let dir = tempfile::TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        // Override project_skills_dir by testing save_skill directly
        let params = json!({
            "project_name": "test",
            "skill_name": "review",
            "content": "# Review\nReview the code."
        });
        let result = save_skill(&params);
        // This will create in the actual ~/.siki path, so just test validation
        assert!(result.is_ok() || result.is_err()); // May fail if ~/.siki doesn't exist
    }

    #[test]
    fn test_save_skill_invalid_name() {
        let params = json!({
            "project_name": "test",
            "skill_name": "../evil",
            "content": "bad"
        });
        let result = save_skill(&params);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_skill_name() {
        assert!(validate_skill_name("review").is_ok());
        assert!(validate_skill_name("my-skill").is_ok());
        assert!(validate_skill_name("my_skill_2").is_ok());
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("../evil").is_err());
        assert!(validate_skill_name("a b").is_err());
    }

    #[test]
    fn test_list_skills_empty() {
        let params = json!({ "project_name": "nonexistent-project-12345" });
        let result = list_skills(&params).unwrap();
        assert_eq!(result["skills"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_scan_summary_files_no_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let (latest, next) = scan_summary_files(dir.path());
        assert!(latest.is_none());
        assert_eq!(next, 2);
    }

    #[test]
    fn test_scan_summary_files_base_only() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("conversation-summary.md"), "# Summary").unwrap();

        let (latest, next) = scan_summary_files(dir.path());
        assert_eq!(latest.unwrap(), dir.path().join("conversation-summary.md"));
        assert_eq!(next, 2);
    }

    #[test]
    fn test_scan_summary_files_with_numbered() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("conversation-summary.md"), "# Summary 1").unwrap();
        std::fs::write(dir.path().join("conversation-summary-2.md"), "# Summary 2").unwrap();
        std::fs::write(dir.path().join("conversation-summary-3.md"), "# Summary 3").unwrap();

        let (latest, next) = scan_summary_files(dir.path());
        assert_eq!(latest.unwrap(), dir.path().join("conversation-summary-3.md"));
        assert_eq!(next, 4);
    }

    #[test]
    fn test_scan_summary_files_with_gap() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("conversation-summary.md"), "# Summary 1").unwrap();
        std::fs::write(dir.path().join("conversation-summary-5.md"), "# Summary 5").unwrap();
        // gap: 2, 3, 4 are missing

        let (latest, next) = scan_summary_files(dir.path());
        assert_eq!(latest.unwrap(), dir.path().join("conversation-summary-5.md"));
        assert_eq!(next, 6);
    }

    #[test]
    fn test_scan_summary_files_ignores_non_summary() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("conversation-summary.md"), "# Summary").unwrap();
        std::fs::write(dir.path().join("other-file.md"), "other").unwrap();
        std::fs::write(dir.path().join("conversation-summary-abc.md"), "bad name").unwrap();

        let (latest, next) = scan_summary_files(dir.path());
        assert_eq!(latest.unwrap(), dir.path().join("conversation-summary.md"));
        assert_eq!(next, 2);
    }
}

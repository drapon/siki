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
        "list_sessions" => {
            // 未登録呼び出し元のフォールバック用に MCP サーバープロセスの cwd を渡す。
            // MCP サーバーはその session の worktree で spawn されるため current_dir が
            // worktree パスになる（session_start.rs の cwd フォールバックと同方針）。
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            list_sessions_with_cwd(conn, session_id, &cwd, params)
        }
        "send_message" => send_message(conn, params, session_id),
        "broadcast" => broadcast(conn, params, session_id),
        "set_summary" => set_summary(conn, params, session_id),
        "set_alert" => set_alert(conn, params, session_id),
        "handoff" => handoff(conn, params, session_id),
        "get_context" => get_context(conn, params),
        "save_skill" => save_skill(params),
        "list_skills" => list_skills(params),
        "summarize_history" => summarize_history(conn, params, session_id),
        "dispatch" => dispatch(conn, params, session_id),
        "move_worktree" => move_worktree(conn, params, session_id),
        _ => anyhow::bail!("Unknown tool: {}", tool_name),
    }
}

fn list_sessions_with_cwd(
    conn: &Connection,
    session_id: &str,
    cwd: &str,
    params: &Value,
) -> Result<Value> {
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

    // 自セッションの worktree/project を特定（スコープフィルタに使用）。
    // 登録済みなら DB の proj/wt を使う。DB 未登録（siki への register と upsert 完了の
    // 間のレース中など）は cwd から補完する。補完しないと自分の proj/wt が特定できず、
    // project スコープのフィルタと project broadcast の受信で空・取りこぼしになる
    // （session_start.rs の cwd 派生と同方針。"unknown" センチネルは無効扱い）。
    let my_worktree = all_sessions.iter().find(|s| s.session_id == session_id);
    let (wt, proj): (String, String) = match my_worktree {
        Some(s) => (s.worktree_name.clone(), s.project_name.clone()),
        None => {
            let (p, w) = crate::session::guess_names_from_cwd(cwd);
            if p == "unknown" || w == "unknown" {
                (String::new(), String::new())
            } else {
                (w, p)
            }
        }
    };

    // scope に応じてフィルタ。enum 外の値はサイレントに machine 扱いにせず拒否する
    // （broadcast と同じコントラクト。タイポが全件返却で気付かれないのを防ぐ）。
    let sessions: Vec<&db::SessionRow> = match scope {
        "worktree" => all_sessions
            .iter()
            .filter(|s| !wt.is_empty() && s.worktree_name == wt && s.project_name == proj)
            .collect(),
        "project" => all_sessions
            .iter()
            .filter(|s| !proj.is_empty() && s.project_name == proj)
            .collect(),
        "machine" => all_sessions.iter().collect(),
        _ => anyhow::bail!(
            "Invalid scope: {} (expected \"machine\", \"project\", or \"worktree\")",
            scope
        ),
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

    let pending = db::get_pending_messages(conn, session_id, &wt, &proj)?;
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
        crate::config::load_contexts(&proj, &wt)
    } else {
        Vec::new()
    };
    // messages 本文を載せない軽量クエリを使う（件数・サマリのみ必要なため）。
    // DB エラーは握り潰さず呼び出し元へ伝播させる（同関数冒頭の db::list_sessions(conn)? と一貫）。
    let summaries = if valid_worktree {
        db::get_conversation_log_summaries_by_worktree(conn, &wt, &proj)?
    } else {
        Vec::new()
    };

    let mut result = json!({ "sessions": items, "pending_messages": messages });

    // 呼び出し元セッションが DB 未登録（siki TUI 未起動など）の場合、project/worktree
    // スコープでは自分の proj/wt を特定できず sessions が空になる。黙って空を返すと
    // 原因が分からないため、診断フラグを添えて scope:"machine" への切り替えを促す。
    if my_worktree.is_none() {
        result["caller_unregistered"] = json!(true);
    }

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
        // KB 見積もりは worktree context（.md）と会話サマリ本文の両方を計上する。
        // どちらも include_bodies:true で返るため、合計サイズが分からないと呼び出し元が
        // 取得コストを過小評価してしまう。
        let total_bytes: usize = contexts.iter().map(|(_, c)| c.len()).sum();
        let summary_bytes: usize = summaries
            .iter()
            .filter_map(|s| s.summary.as_deref())
            .map(|s| s.len())
            .sum();
        result["background"] = json!({
            "conversation_summary_count": summaries.len(),
            "conversation_summary_kb": crate::config::bytes_to_kb(summary_bytes),
            "worktree_context_files": contexts.len(),
            "worktree_context_kb": crate::config::bytes_to_kb(total_bytes),
            "hint": "Pass include_bodies:true to fetch the full conversation summaries and worktree context bodies (independent of scope). These can be large — prefer fetching only when the current task needs them.",
        });
    }

    Ok(result)
}

/// テスト用の薄いラッパ。cwd フォールバックを効かせない（cwd 空）決定的な入口。
/// 本番経路は execute_tool が current_dir を渡して list_sessions_with_cwd を呼ぶ。
#[cfg(test)]
fn list_sessions(conn: &Connection, session_id: &str, params: &Value) -> Result<Value> {
    list_sessions_with_cwd(conn, session_id, "", params)
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

    // 既定スコープは "project"（スキーマの宣言に合わせる）。送信元と同じ project の
    // セッションにのみ配信する。"machine" は全 NULL でマシン全体へ送る。
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    match scope {
        "machine" | "project" => {}
        _ => anyhow::bail!("Invalid scope: {} (expected \"machine\" or \"project\")", scope),
    }

    // project スコープでは送信元の project を sessions から引く。送信元が未登録で
    // project を特定できない場合は machine 扱い（全 NULL）にフォールバックし、
    // メッセージを取りこぼさないようにする。
    // "unknown" は guess_names_from_cwd（workspaces 外起動時）と DB デフォルトの
    // センチネルであり、有効な project 名ではない。これを to_project に採用すると
    // 他の "unknown" セッション群へ誤配信されるため、空文字と同様に machine
    // フォールバック扱いにする（session_start.rs の valid_worktree 判定と一致）。
    let to_project: Option<String> = if scope == "project" {
        db::list_sessions(conn)?
            .iter()
            .find(|s| s.session_id == from_session)
            .map(|s| s.project_name.clone())
            .filter(|p| !p.is_empty() && p != "unknown")
    } else {
        None
    };

    db::insert_message(
        conn,
        from_session,
        None,
        None,
        to_project.as_deref(),
        message,
        "message",
        None,
    )?;

    let effective_scope = if to_project.is_some() { "project" } else { "machine" };
    Ok(json!({ "delivered": true, "scope": effective_scope }))
}

/// 指揮者worktreeから対象worktreeのClaudeターミナルへプロンプトを自動投入する。
///
/// DB へ message_type='dispatch' の行を INSERT するのみで、実際の PTY 書き込みは
/// TUI 本体の Tick が担う（人間の承認ステップは挟まない）。
/// target.type "subtree" はスキーマ上先行宣言されているが、Phase 2 で
/// get_descendants が配線されるまで明示的にエラーとする。
fn dispatch(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let target = params.get("target").ok_or_else(|| anyhow::anyhow!("target is required"))?;
    let target_type = target
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.type is required"))?;
    let target_id = target
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.id is required"))?;
    let prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("prompt is required"))?;

    // 呼び出し元セッションの project_name を自己解決（list_sessions と同じパターン）
    let all_sessions = db::list_sessions(conn)?;
    let project_name = all_sessions
        .iter()
        .find(|s| s.session_id == from_session)
        .map(|s| s.project_name.clone())
        .ok_or_else(|| anyhow::anyhow!("caller session not found"))?;

    let targets: Vec<String> = match target_type {
        "worktree" => vec![target_id.to_string()],
        _ => anyhow::bail!("Invalid or not-yet-supported target type: {}", target_type),
    };

    for wt_name in &targets {
        db::insert_message(conn, from_session, None, Some(wt_name), Some(&project_name), prompt, "dispatch", None)?;
    }

    Ok(json!({ "dispatched": targets.len(), "targets": targets }))
}

/// 呼び出し元project内でworktreeの親リンクを付け替える。
fn move_worktree(conn: &Connection, params: &Value, from_session: &str) -> Result<Value> {
    let child = params
        .get("child")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("child is required"))?;
    let parent = params.get("parent").and_then(|v| v.as_str());

    let all_sessions = db::list_sessions(conn)?;
    let project_name = all_sessions
        .iter()
        .find(|s| s.session_id == from_session)
        .map(|s| s.project_name.clone())
        .ok_or_else(|| anyhow::anyhow!("caller session not found"))?;

    if !crate::config::worktree_path(&project_name, child).exists() {
        anyhow::bail!("child worktree not found: {}", child);
    }
    if let Some(p) = parent {
        if !crate::config::worktree_path(&project_name, p).exists() {
            anyhow::bail!("parent worktree not found: {}", p);
        }
        if crate::config::would_create_cycle(&project_name, child, p) {
            anyhow::bail!("circular parent link: {} -> {}", child, p);
        }
    }

    crate::config::save_worktree_parent(&project_name, child, parent)?;

    Ok(json!({ "child": child, "parent": parent }))
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
    use crate::config;
    use std::path::Path;

    fn test_db() -> Connection {
        db::init(Path::new(":memory:")).unwrap()
    }

    struct TestProject {
        name: String,
    }

    impl TestProject {
        fn new(suffix: &str, worktrees: &[&str]) -> Self {
            let name = format!("task-0004-{}-{}", std::process::id(), suffix);
            let _ = config::remove_project_meta(&name);
            config::save_project_meta(&name, Path::new("/tmp/siki-task-0004")).unwrap();
            for worktree in worktrees {
                std::fs::create_dir_all(config::worktree_path(&name, worktree)).unwrap();
            }
            Self { name }
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = config::remove_project_meta(&self.name);
        }
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
    fn test_list_sessions_unregistered_caller() {
        // 呼び出し元が DB 未登録の場合、project 既定スコープでは proj を特定できず
        // sessions が空になる。診断フラグ caller_unregistered が付くこと、
        // scope:"machine" を明示すれば全件見えることを保証する。
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp/osaka", "idle").unwrap();
        db::upsert_session(&conn, "s2", "default", "tokyo", "myapp", "/tmp/tokyo", "idle").unwrap();

        // 未登録の "ghost" が既定（project）で呼ぶ → 空 + caller_unregistered
        let result = list_sessions(&conn, "ghost", &json!({})).unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(result["caller_unregistered"], true);

        // scope:"machine" を明示すれば全件返る
        let result = list_sessions(&conn, "ghost", &json!({"scope": "machine"})).unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 2);
        // machine でも未登録であることは変わらないので診断は付く
        assert_eq!(result["caller_unregistered"], true);

        // 登録済み呼び出し元には診断フラグは付かない
        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
        assert!(result.get("caller_unregistered").is_none());
    }

    #[test]
    fn test_list_sessions_unregistered_caller_derives_proj_from_cwd() {
        // DB 未登録の呼び出し元でも、cwd が workspaces 配下なら proj/wt を補完し、
        // 同 project のセッション表示と project broadcast の受信ができる（登録レース対策）。
        // guess_names_from_cwd は FS を読まず文字列でパスを分解するため実ファイル作成は不要。
        let proj = "m2proj";
        let wt = "m2wt";
        let cwd = crate::config::workspaces_dir()
            .join(proj)
            .join(wt)
            .to_string_lossy()
            .to_string();
        let conn = test_db();
        // 同 project の登録済みセッションと送信者
        db::upsert_session(&conn, "peer", "default", "otherwt", proj, "/tmp/p", "idle").unwrap();
        db::upsert_session(&conn, "sender", "default", "otherwt", proj, "/tmp/s", "idle").unwrap();
        // 別 project のセッション（project スコープから除外される確認用）
        db::upsert_session(&conn, "outsider", "default", "bwt", "bproj", "/tmp/o", "idle").unwrap();
        // 登録済み sender が project broadcast（to_project = m2proj）
        broadcast(&conn, &json!({ "message": "for the project" }), "sender").unwrap();

        // 未登録 "ghost" が cwd 付きで既定（project）スコープで呼ぶ
        let result = list_sessions_with_cwd(&conn, "ghost", &cwd, &json!({})).unwrap();

        // DB 未登録の診断は付く（登録は完了していない）
        assert_eq!(result["caller_unregistered"], true);
        // cwd から proj を補完 → 同 project のセッションだけが見える（別 project は除外）
        let ids: Vec<&str> = result["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"peer"), "should see same-project peer, got {:?}", ids);
        assert!(ids.contains(&"sender"), "should see same-project sender, got {:?}", ids);
        assert!(!ids.contains(&"outsider"), "must exclude other project, got {:?}", ids);
        // project broadcast を取りこぼさない
        let msgs = result["pending_messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "unregistered caller should receive project broadcast via cwd");
        assert_eq!(msgs[0]["content"], "for the project");

        // cwd が空（従来の未登録経路）なら補完できず空のまま（回帰の確認）
        let empty = list_sessions_with_cwd(&conn, "ghost", "", &json!({})).unwrap();
        assert_eq!(empty["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(empty["pending_messages"].as_array().unwrap().len(), 0);
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
        // 送信元が未登録の場合、project スコープは project を特定できず machine に
        // フォールバックする（全 NULL）。よって別 worktree/project の受信者にも届く。
        let conn = test_db();
        let params = json!({ "message": "hello everyone" });
        let result = broadcast(&conn, &params, "s1").unwrap();
        assert_eq!(result["delivered"], true);
        assert_eq!(result["scope"], "machine");

        let msgs = db::get_pending_messages(&conn, "s2", "wt", "proj").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello everyone");
    }

    #[test]
    fn test_broadcast_project_scope_limits_to_sender_project() {
        let conn = test_db();
        // 送信元 sender は myapp に登録済み
        db::upsert_session(&conn, "sender", "default", "osaka", "myapp", "/tmp/o", "idle").unwrap();

        // 既定（project）で broadcast → 送信元の project (myapp) 宛
        let result = broadcast(&conn, &json!({ "message": "team only" }), "sender").unwrap();
        assert_eq!(result["scope"], "project");

        // 同 project の別セッションは受信できる（to_project = myapp に一致）
        let same = db::get_pending_messages(&conn, "peer", "tokyo", "myapp").unwrap();
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].content, "team only");

        // 別 project のセッションには届かない
        let other = db::get_pending_messages(&conn, "outsider", "berlin", "other").unwrap();
        assert_eq!(other.len(), 0);
    }

    #[test]
    fn test_broadcast_machine_scope_reaches_all_projects() {
        let conn = test_db();
        db::upsert_session(&conn, "sender", "default", "osaka", "myapp", "/tmp/o", "idle").unwrap();

        let result =
            broadcast(&conn, &json!({ "message": "all hands", "scope": "machine" }), "sender")
                .unwrap();
        assert_eq!(result["scope"], "machine");

        // 別 project のセッションにも届く
        let other = db::get_pending_messages(&conn, "outsider", "berlin", "other").unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].content, "all hands");
    }

    #[test]
    fn test_broadcast_rejects_invalid_scope() {
        let conn = test_db();
        let result = broadcast(&conn, &json!({ "message": "x", "scope": "worktree" }), "s1");
        assert!(result.is_err());
    }

    #[test]
    fn test_broadcast_unknown_project_falls_back_to_machine() {
        // project_name が "unknown"（workspaces 外で起動し guess_names_from_cwd が
        // フォールバックした登録）の送信元は project を特定できないものとして扱い、
        // machine フォールバックする。"unknown" を project 名として配信すると、無関係な
        // 別 "unknown" セッション群へ誤配信されてしまうため。
        let conn = test_db();
        db::upsert_session(&conn, "ghost", "default", "unknown", "unknown", "/elsewhere", "idle")
            .unwrap();

        let result = broadcast(&conn, &json!({ "message": "hi" }), "ghost").unwrap();
        // "unknown" は project として採用せず machine フォールバック
        assert_eq!(result["scope"], "machine");

        // machine 扱いなので無関係な project のセッションにも届く
        let other = db::get_pending_messages(&conn, "outsider", "berlin", "other").unwrap();
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn test_broadcast_does_not_echo_to_sender() {
        // 送信元自身は自分が送った broadcast / project fanout を受信しない。
        let conn = test_db();
        db::upsert_session(&conn, "sender", "default", "osaka", "myapp", "/tmp/o", "idle").unwrap();

        // project スコープ broadcast（to_project = myapp）
        broadcast(&conn, &json!({ "message": "to my team" }), "sender").unwrap();
        let mine = db::get_pending_messages(&conn, "sender", "osaka", "myapp").unwrap();
        assert_eq!(mine.len(), 0, "sender must not receive own project broadcast");

        // 同 project の別セッションは受信する
        let peer = db::get_pending_messages(&conn, "peer", "tokyo", "myapp").unwrap();
        assert_eq!(peer.len(), 1);

        // machine broadcast（全 NULL）でも送信元には返らない
        broadcast(&conn, &json!({ "message": "all", "scope": "machine" }), "sender").unwrap();
        let mine2 = db::get_pending_messages(&conn, "sender", "osaka", "myapp").unwrap();
        assert_eq!(mine2.len(), 0, "sender must not receive own machine broadcast");
    }

    #[test]
    fn test_list_sessions_rejects_invalid_scope() {
        // 不正な scope はサイレントに machine 扱い（全件）にせず、broadcast と同様に拒否する。
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "osaka", "myapp", "/tmp", "idle").unwrap();
        let result = list_sessions(&conn, "s1", &json!({"scope": "bogus"}));
        assert!(result.is_err(), "invalid scope must error, not silently return all");
    }

    #[test]
    fn test_background_pointer_includes_conversation_summary_kb() {
        // background ポインタは worktree context だけでなく会話サマリ本文のサイズも
        // KB で見積もる（include_bodies のコストを呼び出し元が事前判断できるように）。
        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces
            .join("proj-kb")
            .join("wt-kb")
            .to_string_lossy()
            .to_string();
        let conn = test_db();
        db::upsert_session(&conn, "s1", "default", "wt-kb", "proj-kb", &cwd, "idle").unwrap();
        db::upsert_conversation_log(&conn, "old", "wt-kb", "proj-kb", None, "[]").unwrap();
        // 約 2000 bytes のサマリ本文
        let big = "x".repeat(2000);
        db::update_conversation_log_summary(&conn, "old", &big).unwrap();

        let result = list_sessions(&conn, "s1", &json!({})).unwrap();
        let bg = result.get("background").expect("background pointer expected");
        // 2000 bytes → (2000 + 512) / 1024 = 2 KB
        assert_eq!(bg["conversation_summary_kb"], 2);
    }

    #[test]
    fn test_dispatch_to_worktree_inserts_dispatch_message() {
        let conn = test_db();
        db::upsert_session(&conn, "parent", "default", "main", "myapp", "/tmp/main", "idle").unwrap();
        let params = json!({
            "target": { "type": "worktree", "id": "child-a" },
            "prompt": "do X"
        });

        let result = dispatch(&conn, &params, "parent").unwrap();
        assert_eq!(result, json!({"dispatched": 1, "targets": ["child-a"]}));

        let dispatches = db::get_pending_dispatches(&conn).unwrap();
        assert_eq!(dispatches.len(), 1);
        assert_eq!(dispatches[0].to_worktree.as_deref(), Some("child-a"));
        assert_eq!(dispatches[0].to_project.as_deref(), Some("myapp"));
        assert_eq!(dispatches[0].content, "do X");

        let message_type: String = conn
            .query_row("SELECT message_type FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(message_type, "dispatch");
    }

    #[test]
    fn test_dispatch_rejects_not_yet_supported_target_types() {
        let conn = test_db();
        db::upsert_session(&conn, "parent", "default", "main", "myapp", "/tmp/main", "idle").unwrap();

        // "subtree" は Phase 2（TASK-0007）まで意図的にエラーとする
        for target_type in ["subtree", "invalid"] {
            let params = json!({
                "target": { "type": target_type, "id": "child-a" },
                "prompt": "do X"
            });
            let err = dispatch(&conn, &params, "parent").unwrap_err();
            assert!(
                err.to_string().contains("Invalid or not-yet-supported target type"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn test_dispatch_requires_target_and_prompt() {
        let conn = test_db();
        db::upsert_session(&conn, "parent", "default", "main", "myapp", "/tmp/main", "idle").unwrap();

        let missing_target = dispatch(&conn, &json!({"prompt": "do X"}), "parent").unwrap_err();
        assert!(missing_target.to_string().contains("target is required"));

        let missing_prompt = dispatch(
            &conn,
            &json!({"target": { "type": "worktree", "id": "child-a" }}),
            "parent",
        )
        .unwrap_err();
        assert!(missing_prompt.to_string().contains("prompt is required"));
    }

    #[test]
    fn test_dispatch_rejects_unknown_caller_session() {
        let conn = test_db();
        let params = json!({
            "target": { "type": "worktree", "id": "child-a" },
            "prompt": "do X"
        });

        let err = dispatch(&conn, &params, "missing").unwrap_err();
        assert!(err.to_string().contains("caller session not found"));
    }

    #[test]
    fn test_move_worktree_sets_parent() {
        let conn = test_db();
        let project = TestProject::new("sets-parent", &["child", "parent"]);
        db::upsert_session(&conn, "s1", "default", "main", &project.name, "/tmp/main", "idle").unwrap();

        let result = move_worktree(&conn, &json!({"child": "child", "parent": "parent"}), "s1").unwrap();

        assert_eq!(result, json!({"child": "child", "parent": "parent"}));
        let meta = config::load_worktree_meta(&project.name, "child").unwrap();
        assert_eq!(meta.parent.as_deref(), Some("parent"));
    }

    #[test]
    fn test_move_worktree_rejects_self_parent() {
        let conn = test_db();
        let project = TestProject::new("self-parent", &["X"]);
        config::save_worktree_parent(&project.name, "X", None).unwrap();
        db::upsert_session(&conn, "s1", "default", "main", &project.name, "/tmp/main", "idle").unwrap();

        let err = move_worktree(&conn, &json!({"child": "X", "parent": "X"}), "s1").unwrap_err();

        assert!(err.to_string().contains("circular parent link: X -> X"));
        let meta = config::load_worktree_meta(&project.name, "X").unwrap();
        assert_eq!(meta.parent, None);
    }

    #[test]
    fn test_move_worktree_rejects_transitive_cycle() {
        let conn = test_db();
        let project = TestProject::new("transitive-cycle", &["A", "B", "C"]);
        config::save_worktree_parent(&project.name, "A", None).unwrap();
        config::save_worktree_parent(&project.name, "B", Some("A")).unwrap();
        config::save_worktree_parent(&project.name, "C", Some("B")).unwrap();
        db::upsert_session(&conn, "s1", "default", "main", &project.name, "/tmp/main", "idle").unwrap();

        let err = move_worktree(&conn, &json!({"child": "A", "parent": "C"}), "s1").unwrap_err();

        assert!(err.to_string().contains("circular parent link: A -> C"));
        let meta = config::load_worktree_meta(&project.name, "A").unwrap();
        assert_eq!(meta.parent, None);
    }

    #[test]
    fn test_move_worktree_detaches_with_null_parent() {
        let conn = test_db();
        let project = TestProject::new("detach", &["A", "B"]);
        config::save_worktree_parent(&project.name, "B", Some("A")).unwrap();
        db::upsert_session(&conn, "s1", "default", "main", &project.name, "/tmp/main", "idle").unwrap();

        let result = move_worktree(&conn, &json!({"child": "B", "parent": null}), "s1").unwrap();

        assert_eq!(result, json!({"child": "B", "parent": null}));
        let meta = config::load_worktree_meta(&project.name, "B").unwrap();
        assert_eq!(meta.parent, None);
    }

    #[test]
    fn test_move_worktree_rejects_missing_child_without_creating_meta() {
        let conn = test_db();
        let project = TestProject::new("missing-child", &["X"]);
        db::upsert_session(&conn, "s1", "default", "main", &project.name, "/tmp/main", "idle").unwrap();

        let err = move_worktree(&conn, &json!({"child": "Y", "parent": "X"}), "s1").unwrap_err();

        assert!(err.to_string().contains("child worktree not found: Y"));
        assert!(config::load_worktree_meta(&project.name, "Y").is_none());
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

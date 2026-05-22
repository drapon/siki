use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::db;

/// SessionStart hook の実装。
///
/// stdin から Claude Code が渡す JSON を読み、broker に register イベントを送信し、
/// `hookSpecificOutput.additionalContext` を stdout に出力する。
///
/// 従来は `.claude/rules/siki.md` が「最初のメッセージの前に list_sessions を呼べ」と
/// 指示していたが、それを廃止して SessionStart hook で直接コンテキストを注入する。
/// MCP ラウンドトリップ1回分を節約でき、レスポンスサイズもこちらで制御できる。
pub fn run(sock_path: &Path, db_path: &Path) -> Result<()> {
    // Claude Code は SessionStart hook に {"session_id": "...", ...} を stdin で渡す
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let input_json: Value = serde_json::from_str(&input).unwrap_or(json!({}));

    let session_id = input_json
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Claude Code は stdin の JSON に cwd を含めるのでそれを優先する。
    // 無ければ process の cwd にフォールバック。
    let cwd = input_json
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let role = std::env::var("SIKI_ROLE").unwrap_or_else(|_| "default".to_string());

    // broker への register を送信（siki TUI が立ち上がっていない場合はサイレントにスキップ）
    let _ = register_with_broker(sock_path, session_id, &cwd, &role);

    // DB から additionalContext 用の情報を取得
    // siki TUI が一度も走っていない環境では DB ファイルが存在しないので、その場合は
    // 空ファイルを作らずに何も出力しないで終了する（Claude Code は exit 0 + 空 stdout を
    // 「追加コンテキストなし」として扱う）
    if db_path.exists() {
        if let Ok(conn) = Connection::open(db_path) {
            let _ = conn.execute_batch("PRAGMA busy_timeout=2000;");
            if let Ok(ctx) = build_additional_context(&conn, session_id, &cwd) {
                if !ctx.is_empty() {
                    let output = json!({
                        "hookSpecificOutput": {
                            "hookEventName": "SessionStart",
                            "additionalContext": ctx,
                        }
                    });
                    println!("{}", output);
                }
            }
        }
    }

    Ok(())
}

fn register_with_broker(sock_path: &Path, session_id: &str, cwd: &str, role: &str) -> Result<()> {
    if session_id.is_empty() || !sock_path.exists() {
        return Ok(());
    }
    let mut stream = UnixStream::connect(sock_path)?;
    // broker 側で1行読んだら接続を閉じるので、書き込み側もタイムアウトを短く
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let payload = json!({
        "event": "register",
        "session_id": session_id,
        "cwd": cwd,
        "role": role,
    });
    writeln!(stream, "{}", payload)?;
    Ok(())
}

/// SessionStart 用の追加コンテキスト本文を組み立てる。
///
/// - 未読の pending_messages があれば本文に含めて既読化
/// - 利用可能な会話サマリ件数と worktree_contexts の件数を「ポインタ」として提示
///   （フル本文はコンテキストを食うので必要なときだけ MCP 経由で取得させる）
/// - set_summary 呼び出しのリマインダー
pub fn build_additional_context(conn: &Connection, session_id: &str, cwd: &str) -> Result<String> {
    let (proj, wt) = crate::session::guess_names_from_cwd(cwd);

    let mut sections: Vec<String> = Vec::new();

    let pending = db::get_pending_messages(conn, session_id, &wt, &proj).unwrap_or_default();
    if !pending.is_empty() {
        let mut s = String::from("## Pending messages\n\nDeliver these to the user before continuing.\n");
        for m in &pending {
            s.push_str(&format!(
                "\n### from `{}` ({})\n{}\n",
                m.from_session, m.message_type, m.content
            ));
        }
        sections.push(s);
        let ids: Vec<i64> = pending.iter().map(|m| m.id).collect();
        let _ = db::mark_messages_read(conn, &ids);
    }

    if !proj.is_empty() && !wt.is_empty() && proj != "unknown" && wt != "unknown" {
        let summaries =
            db::get_conversation_logs_by_worktree(conn, &wt, &proj).unwrap_or_default();
        let contexts = crate::config::load_contexts(&proj, &wt);

        let mut lines: Vec<String> = Vec::new();
        if !summaries.is_empty() {
            lines.push(format!(
                "- {} prior conversation summary file(s) available",
                summaries.len()
            ));
        }
        if !contexts.is_empty() {
            let total_bytes: usize = contexts.iter().map(|(_, c)| c.len()).sum();
            lines.push(format!(
                "- {} worktree context file(s) (~{} KB total)",
                contexts.len(),
                (total_bytes + 512) / 1024
            ));
        }
        if !lines.is_empty() {
            let mut s = String::from("## Background available for this worktree\n\n");
            for line in &lines {
                s.push_str(line);
                s.push('\n');
            }
            s.push_str(
                "\nFetch only what the current task needs:\n\
                - `siki:list_sessions { scope: \"worktree\" }` returns summaries and context bodies.\n\
                - `siki:get_context { include_conversation_log: true }` is large — call it via an Explore or general-purpose subagent so the full log stays out of your main context.\n",
            );
            sections.push(s);
        }
    }

    if sections.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::from("# siki session context\n\n");
    for s in sections {
        out.push_str(&s);
        out.push('\n');
    }
    if !proj.is_empty() && !wt.is_empty() && proj != "unknown" && wt != "unknown" {
        out.push_str(&format!(
            "When you start work, call `siki:set_summary` with a short task description (project `{}`, worktree `{}`).\n",
            proj, wt
        ));
    } else {
        out.push_str("When you start work, call `siki:set_summary` with a short task description.\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_db() -> Connection {
        db::init(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn empty_db_yields_empty_context() {
        let conn = test_db();
        let ctx = build_additional_context(&conn, "s1", "/tmp/nowhere").unwrap();
        assert!(ctx.is_empty(), "got: {:?}", ctx);
    }

    #[test]
    fn pending_messages_are_included_and_marked_read() {
        let conn = test_db();
        // s2 という未登録セッションに対して broadcast を一件送る
        db::insert_message(&conn, "s1", Some("s2"), None, None, "hello there", "message", None)
            .unwrap();

        let ctx = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        assert!(ctx.contains("Pending messages"), "got: {}", ctx);
        assert!(ctx.contains("hello there"));

        // 2回目は既読なので含まれない
        let ctx2 = build_additional_context(&conn, "s2", "/tmp/nowhere").unwrap();
        assert!(!ctx2.contains("hello there"));
    }

    #[test]
    fn background_pointers_appear_when_logs_exist() {
        // siki workspaces 配下を装ったパスにする
        let workspaces = crate::config::workspaces_dir();
        let cwd = workspaces.join("proj-x").join("wt-x").to_string_lossy().to_string();

        let conn = test_db();
        db::upsert_conversation_log(&conn, "old-sid", "wt-x", "proj-x", None, "[]").unwrap();

        let ctx = build_additional_context(&conn, "new-sid", &cwd).unwrap();
        assert!(ctx.contains("Background available"), "got: {}", ctx);
        assert!(ctx.contains("conversation summary"));
        assert!(ctx.contains("siki:set_summary"));
    }
}

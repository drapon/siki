pub mod protocol;
pub mod tools;

use anyhow::Result;
use serde_json::json;
use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::db;

/// MCP stdio サーバーを起動する（`siki mcp` サブコマンドから呼ばれる）
pub fn run_stdio_server(db_path: &Path) -> Result<()> {
    let conn = db::init(db_path)?;

    // セッション ID を環境変数から取得
    let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_else(|_| "unknown".to_string());

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: protocol::JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(_) => {
                let resp = protocol::JsonRpcResponse::error(
                    None,
                    -32700,
                    "Parse error".to_string(),
                );
                write_response(&mut stdout, &resp)?;
                continue;
            }
        };

        let response = handle_request(&conn, &request, &session_id);

        // notification（id が None）にはレスポンスを返さない
        if request.id.is_some() {
            write_response(&mut stdout, &response)?;
        }
    }

    Ok(())
}

fn handle_request(
    conn: &rusqlite::Connection,
    request: &protocol::JsonRpcRequest,
    session_id: &str,
) -> protocol::JsonRpcResponse {
    match request.method.as_str() {
        "initialize" => protocol::JsonRpcResponse::success(
            request.id.clone(),
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "siki",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": "You are connected to siki, a multi-session orchestrator.\n\nThe SessionStart hook already injects pending messages and a pointer to available background at the start of each conversation, so you do NOT need to call `list_sessions` first thing.\n\n- Call `set_summary` with a short task description once work begins.\n- Call `list_sessions` only when you actually need to see other sessions (defaults to the current project). It returns background (prior conversation summaries, worktree context files) as counts only; pass `include_bodies:true` to fetch the full bodies, and only when the current task needs them.\n- For full conversation details from past sessions, use `get_context` with `include_conversation_log: true` (large — delegate to a subagent)."
            }),
        ),

        "notifications/initialized" => {
            // notification — レスポンス不要
            protocol::JsonRpcResponse::success(request.id.clone(), json!({}))
        }

        "tools/list" => {
            let tools = protocol::tool_definitions();
            protocol::JsonRpcResponse::success(
                request.id.clone(),
                json!({ "tools": tools }),
            )
        }

        "tools/call" => {
            let tool_name = request
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));

            match tools::execute_tool(conn, tool_name, &arguments, session_id) {
                Ok(result) => protocol::JsonRpcResponse::success(
                    request.id.clone(),
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&result).unwrap_or_default()
                        }]
                    }),
                ),
                Err(e) => protocol::JsonRpcResponse::success(
                    request.id.clone(),
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Error: {}", e)
                        }],
                        "isError": true
                    }),
                ),
            }
        }

        _ => protocol::JsonRpcResponse::error(
            request.id.clone(),
            -32601,
            format!("Method not found: {}", request.method),
        ),
    }
}

fn write_response(stdout: &mut io::Stdout, response: &protocol::JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(response)?;
    writeln!(stdout, "{}", json)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_conn() -> rusqlite::Connection {
        db::init(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn test_handle_initialize() {
        let conn = test_conn();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: json!({}),
        };
        let resp = handle_request(&conn, &req, "test");
        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "siki");
    }

    #[test]
    fn test_handle_tools_list() {
        let conn = test_conn();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "tools/list".to_string(),
            params: json!({}),
        };
        let resp = handle_request(&conn, &req, "test");
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 13);
    }

    #[test]
    fn test_handle_tools_call_list_sessions() {
        let conn = test_conn();
        db::upsert_session(&conn, "s1", "default", "wt", "proj", "/tmp", "idle").unwrap();
        // 既定スコープは project。呼び出し元 "test" を同 project に登録しておく。
        db::upsert_session(&conn, "test", "default", "wt", "proj", "/tmp", "idle").unwrap();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "tools/call".to_string(),
            params: json!({
                "name": "list_sessions",
                "arguments": {}
            }),
        };
        let resp = handle_request(&conn, &req, "test");
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("s1"));
    }

    #[test]
    fn test_handle_tools_call_send_message() {
        let conn = test_conn();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "tools/call".to_string(),
            params: json!({
                "name": "send_message",
                "arguments": {
                    "target": { "type": "session", "id": "s2" },
                    "message": "test message"
                }
            }),
        };
        let resp = handle_request(&conn, &req, "s1");
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_handle_unknown_method() {
        let conn = test_conn();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(5)),
            method: "unknown/method".to_string(),
            params: json!({}),
        };
        let resp = handle_request(&conn, &req, "test");
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_handle_tools_call_unknown_tool() {
        let conn = test_conn();
        let req = protocol::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(6)),
            method: "tools/call".to_string(),
            params: json!({
                "name": "nonexistent_tool",
                "arguments": {}
            }),
        };
        let resp = handle_request(&conn, &req, "test");
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }
}

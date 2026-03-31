use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC リクエスト
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC レスポンス
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC エラー
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// MCP ツール定義
#[derive(Debug, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// MCP ツール一覧
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "list_sessions".to_string(),
            description: "List all active siki-managed Claude Code sessions".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "enum": ["machine", "project", "worktree"],
                        "description": "Filter scope (default: machine)"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "send_message".to_string(),
            description: "Send a message to another session, worktree, or project".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["session", "worktree", "project"] },
                            "id": { "type": "string" }
                        },
                        "required": ["type", "id"]
                    },
                    "message": { "type": "string" }
                },
                "required": ["target", "message"]
            }),
        },
        ToolDefinition {
            name: "broadcast".to_string(),
            description: "Broadcast a message to all sessions".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" },
                    "scope": {
                        "type": "string",
                        "enum": ["machine", "project"],
                        "description": "Broadcast scope (default: project)"
                    }
                },
                "required": ["message"]
            }),
        },
        ToolDefinition {
            name: "set_summary".to_string(),
            description: "Set a work summary for the current session".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" }
                },
                "required": ["summary"]
            }),
        },
        ToolDefinition {
            name: "handoff".to_string(),
            description: "Hand off context to another session with auto-collected git info".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["session", "worktree", "project"] },
                            "id": { "type": "string" }
                        },
                        "required": ["type", "id"]
                    },
                    "note": {
                        "type": "string",
                        "description": "Optional note to include with the handoff"
                    }
                },
                "required": ["target"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn test_success_response() {
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_error_response() {
        let resp = JsonRpcResponse::error(Some(serde_json::json!(1)), -32600, "Invalid request".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn test_tool_definitions_count() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 5);
    }

    #[test]
    fn test_tool_definition_names() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"list_sessions"));
        assert!(names.contains(&"send_message"));
        assert!(names.contains(&"broadcast"));
        assert!(names.contains(&"set_summary"));
        assert!(names.contains(&"handoff"));
    }
}

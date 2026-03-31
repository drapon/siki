use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// worktree の `.claude/settings.json` に siki 用 hook を注入する
///
/// 既存の設定がある場合はマージし、siki の hook を追加・更新する。
/// 既存の hook は保持される。
pub fn ensure_hooks_configured(worktree_path: &Path, sock_path: &Path) -> Result<()> {
    let claude_dir = worktree_path.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .with_context(|| format!(".claude ディレクトリの作成に失敗: {}", claude_dir.display()))?;

    let settings_path = claude_dir.join("settings.json");
    let mut settings = load_or_default(&settings_path);

    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));

    let sock = sock_path.to_string_lossy();

    inject_hook(hooks, "SessionStart", &format!(
        "echo '{{\"event\":\"register\",\"session_id\":\"'\"$CLAUDE_SESSION_ID\"'\",\"cwd\":\"'\"$PWD\"'\",\"role\":\"'\"${{SIKI_ROLE:-default}}\"'\"}}' | nc -U {sock}"
    ), false);

    inject_hook(hooks, "PreToolUse", &format!(
        "echo '{{\"event\":\"working\",\"session_id\":\"'\"$CLAUDE_SESSION_ID\"'\"}}' | nc -U {sock}"
    ), true);

    inject_hook(hooks, "PermissionRequest", &format!(
        "echo '{{\"event\":\"waiting\",\"session_id\":\"'\"$CLAUDE_SESSION_ID\"'\"}}' | nc -U {sock}"
    ), true);

    // PostToolUse では idle にしない — Stop まで working を維持する

    inject_hook(hooks, "Stop", &format!(
        "echo '{{\"event\":\"idle\",\"session_id\":\"'\"$CLAUDE_SESSION_ID\"'\"}}' | nc -U {sock}"
    ), true);

    inject_hook(hooks, "SessionEnd", &format!(
        "echo '{{\"event\":\"dead\",\"session_id\":\"'\"$CLAUDE_SESSION_ID\"'\"}}' | nc -U {sock}"
    ), true);

    // siki MCP サーバーを自動登録
    inject_mcp_server(&mut settings);

    let content = serde_json::to_string_pretty(&settings)
        .context("settings.json のシリアライズに失敗")?;
    std::fs::write(&settings_path, content)
        .with_context(|| format!("settings.json の書き込みに失敗: {}", settings_path.display()))?;

    Ok(())
}

/// siki MCP サーバーの設定を注入する
///
/// 実行中の siki バイナリのパスを自動検出し、mcpServers に登録する。
fn inject_mcp_server(settings: &mut Value) {
    let siki_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "siki".to_string());

    let mcp_servers = settings
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));

    let servers = match mcp_servers.as_object_mut() {
        Some(s) => s,
        None => return,
    };

    // 既に siki が登録済みなら上書きしない
    if servers.contains_key("siki") {
        return;
    }

    servers.insert(
        "siki".to_string(),
        json!({
            "type": "stdio",
            "command": siki_path,
            "args": ["mcp"]
        }),
    );
}

/// settings.json を読み込む。なければ空オブジェクトを返す。
fn load_or_default(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

/// hook イベントに siki 用コマンドを注入する
///
/// 既存の hook 配列がある場合、siki のコマンドが含まれていなければ追加する。
/// `is_async` が true の場合、"timeout" フィールドを設定してブロックしないようにする。
fn inject_hook(hooks: &mut Value, event: &str, command: &str, is_async: bool) {
    let hooks_obj = hooks.as_object_mut().unwrap();

    let event_hooks = hooks_obj
        .entry(event)
        .or_insert_with(|| json!([]));

    let arr = match event_hooks.as_array_mut() {
        Some(arr) => arr,
        None => return,
    };

    // 既に siki の hook が含まれているか確認（nc -U と siki/sock を含むもの）
    let already_exists = arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains("nc -U") && c.contains(".siki"))
                })
            })
            .unwrap_or(false)
    });

    if already_exists {
        return;
    }

    let mut hook_entry = json!({
        "type": "command",
        "command": command,
    });

    if is_async {
        hook_entry
            .as_object_mut()
            .unwrap()
            .insert("timeout".to_string(), json!(5000));
    }

    arr.push(json!({
        "hooks": [hook_entry]
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_hook_to_empty() {
        let mut hooks = json!({});
        inject_hook(&mut hooks, "SessionStart", "echo test", false);

        let arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["hooks"][0]["command"], "echo test");
    }

    #[test]
    fn test_inject_hook_preserves_existing() {
        let mut hooks = json!({
            "SessionStart": [{
                "hooks": [{"type": "command", "command": "echo existing"}]
            }]
        });
        inject_hook(&mut hooks, "SessionStart", "echo new | nc -U /tmp/.siki/sock", false);

        let arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["hooks"][0]["command"], "echo existing");
    }

    #[test]
    fn test_inject_hook_no_duplicate() {
        let mut hooks = json!({
            "PreToolUse": [{
                "hooks": [{"type": "command", "command": "echo test | nc -U /home/.siki/sock"}]
            }]
        });
        inject_hook(&mut hooks, "PreToolUse", "echo new | nc -U /home/.siki/sock", true);

        let arr = hooks["PreToolUse"].as_array().unwrap();
        // 既に siki の hook があるので追加されない
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn test_inject_hook_async_has_timeout() {
        let mut hooks = json!({});
        inject_hook(&mut hooks, "PreToolUse", "echo test", true);

        let hook = &hooks["PreToolUse"][0]["hooks"][0];
        assert_eq!(hook["timeout"], 5000);
    }

    #[test]
    fn test_inject_hook_sync_no_timeout() {
        let mut hooks = json!({});
        inject_hook(&mut hooks, "SessionStart", "echo test", false);

        let hook = &hooks["SessionStart"][0]["hooks"][0];
        assert!(hook.get("timeout").is_none());
    }

    #[test]
    fn test_ensure_hooks_configured() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        ensure_hooks_configured(dir.path(), &sock_path).unwrap();

        let settings_path = dir.path().join(".claude/settings.json");
        assert!(settings_path.exists());

        let content = std::fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // 各イベントに hook が存在する
        assert!(settings["hooks"]["SessionStart"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["PreToolUse"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["PermissionRequest"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["Stop"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["SessionEnd"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn test_ensure_hooks_preserves_existing_settings() {
        let dir = tempfile::TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();

        // 既存の settings.json を書き込む
        let existing = json!({
            "permissions": {"allow": ["Bash(*)"]},
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "echo existing"}]
                }]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let sock_path = dir.path().join("test.sock");
        ensure_hooks_configured(dir.path(), &sock_path).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // 既存の permissions が保持されている
        assert!(settings["permissions"]["allow"].as_array().unwrap().len() > 0);

        // 既存の hook が保持され、siki の hook が追加されている
        let session_start = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2);
        assert_eq!(session_start[0]["hooks"][0]["command"], "echo existing");
    }

    #[test]
    fn test_load_or_default_missing_file() {
        let v = load_or_default(Path::new("/nonexistent/settings.json"));
        assert_eq!(v, json!({}));
    }

    #[test]
    fn test_load_or_default_invalid_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json {{{").unwrap();
        let v = load_or_default(&path);
        assert_eq!(v, json!({}));
    }
}

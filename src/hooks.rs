use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// worktree の `.claude/settings.json` に siki 用 hook を注入する
///
/// 既存の設定がある場合はマージし、siki の hook を追加・更新する。
/// 既存の hook は保持される。
pub fn ensure_hooks_configured(worktree_path: &Path, sock_path: &Path, project_name: &str) -> Result<()> {
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

    // stdin の JSON から session_id を抽出する共通スクリプト
    // Claude Code はすべての hook に {"session_id": "...", ...} を stdin で渡す
    // ソケットが存在しない場合はサイレントにスキップ（siki 未起動時のエラー抑止）
    let read_sid = format!(
        r#"SID=$(head -1 | sed -n 's/.*"session_id" *: *"\([^"]*\)".*/\1/p'); [ -z "$SID" ] && SID="siki-$$"; [ -S {sock} ] || exit 0"#
    );

    // SessionStart は siki バイナリのサブコマンド経由で実行する。
    // - broker への register イベント送信
    // - DB から pending_messages / 利用可能な背景情報を取得して additionalContext を返す
    // これにより、最初のメッセージで `list_sessions` MCP を呼ばせる必要がなくなる。
    let siki_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "siki".to_string());
    inject_hook(hooks, "SessionStart", &format!(r#"{siki_path} session-start"#), false);

    inject_hook(hooks, "PreToolUse", &format!(
        r#"{read_sid}; echo '{{"event":"working","session_id":"'"$SID"'"}}' | nc -U {sock}"#
    ), true);

    inject_hook(hooks, "PermissionRequest", &format!(
        r#"{read_sid}; echo '{{"event":"waiting","session_id":"'"$SID"'"}}' | nc -U {sock}"#
    ), true);

    // PostToolUse で Changes の再読み込みをトリガーする
    inject_hook(hooks, "PostToolUse", &format!(
        r#"{read_sid}; echo '{{"event":"refresh","session_id":"'"$SID"'"}}' | nc -U {sock}"#
    ), true);

    inject_hook(hooks, "Stop", &format!(
        r#"{read_sid}; echo '{{"event":"idle","session_id":"'"$SID"'"}}' | nc -U {sock}"#
    ), true);

    inject_hook(hooks, "SessionEnd", &format!(
        r#"{read_sid}; echo '{{"event":"dead","session_id":"'"$SID"'"}}' | nc -U {sock}"#
    ), true);

    // worktree ルートに .mcp.json を作成して siki MCP サーバーを登録
    inject_mcp_json(worktree_path);

    // .claude/rules/siki.md にセッション開始時のルールを書き込む
    inject_rules(&claude_dir);

    let content = serde_json::to_string_pretty(&settings)
        .context("settings.json のシリアライズに失敗")?;
    std::fs::write(&settings_path, content)
        .with_context(|| format!("settings.json の書き込みに失敗: {}", settings_path.display()))?;

    // プロジェクト固有の Claude Code skills を同期
    sync_project_skills(worktree_path, project_name);

    // siki が生成したファイルを git 追跡から除外
    exclude_from_git(worktree_path, ".mcp.json");
    exclude_from_git(worktree_path, ".claude/settings.json");
    exclude_from_git(worktree_path, ".claude/rules/siki.md");
    exclude_from_git(worktree_path, ".claude/siki-handoff.md");

    Ok(())
}

/// worktree ルートに .mcp.json を作成して siki MCP サーバーを登録する
///
/// Claude Code は .claude/settings.json ではなく .mcp.json から MCP 設定を読む。
/// セッション開始前に存在している必要がある。
fn inject_mcp_json(worktree_path: &Path) {
    let mcp_path = worktree_path.join(".mcp.json");

    let mut config = std::fs::read_to_string(&mcp_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({"mcpServers": {}}));

    let servers = config
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));

    let servers = match servers.as_object_mut() {
        Some(s) => s,
        None => return,
    };

    if servers.contains_key("siki") {
        return;
    }

    let siki_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "siki".to_string());

    servers.insert(
        "siki".to_string(),
        json!({
            "type": "stdio",
            "command": siki_path,
            "args": ["mcp"]
        }),
    );

    if let Ok(content) = serde_json::to_string_pretty(&config) {
        let _ = std::fs::write(&mcp_path, content);
    }
}

/// ~/.siki/workspaces/<project>/skills/<name>/ ディレクトリを
/// worktree の .claude/skills/<name> にシンボリックリンクする
pub fn sync_project_skills(worktree_path: &Path, project_name: &str) {
    let skills_src = crate::config::project_skills_dir(project_name);
    if !skills_src.is_dir() {
        return;
    }

    let skills_dst = worktree_path.join(".claude").join("skills");
    let _ = std::fs::create_dir_all(&skills_dst);

    let entries = match std::fs::read_dir(&skills_src) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // ディレクトリのみ対象（skills/<name>/SKILL.md 形式）
        if !path.is_dir() {
            continue;
        }
        // SKILL.md が存在しないディレクトリはスキップ
        if !path.join("SKILL.md").exists() {
            continue;
        }
        let dir_name = match path.file_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let dst = skills_dst.join(&dir_name);

        // リポジトリ側に同名ディレクトリが既にあればスキップ（リポジトリ側優先）
        if dst.exists() && !dst.is_symlink() {
            continue;
        }

        // 既存のシンボリックリンクは再作成
        if dst.is_symlink() {
            let _ = std::fs::remove_file(&dst);
        }

        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&path, &dst);
        }

        // シンボリックリンクを git 追跡から除外
        let relative = format!(".claude/skills/{}", dir_name.to_string_lossy());
        exclude_from_git(worktree_path, &relative);
    }
}

/// .claude/rules/siki.md にセッション開始ルールを書き込む
///
/// siki が以前書き込んだファイル（HTML マーカーまたは旧版の固有フレーズで識別）は
/// 上書きし、ユーザが置き換えた他の内容は保持する。
fn inject_rules(claude_dir: &Path) {
    let rules_dir = claude_dir.join("rules");
    let _ = std::fs::create_dir_all(&rules_dir);
    let rules_path = rules_dir.join("siki.md");

    // 既存ファイルがあって、かつ siki 管理であると判定できない場合は触らない
    if let Ok(existing) = std::fs::read_to_string(&rules_path) {
        if !is_siki_managed_rules(&existing) {
            return;
        }
    }

    let content = format!(
        "{}\n# siki session rules\n\n{}",
        SIKI_MANAGED_RULES_MARKER, SIKI_RULES_BODY
    );

    let _ = std::fs::write(&rules_path, content);
}

/// siki 管理 siki.md を識別するためのマーカー。本文の冒頭に出力される。
/// 改版時はバージョン番号を上げる（既存マーカーも検出される）。
const SIKI_MANAGED_RULES_MARKER: &str = "<!-- siki-managed-rules: v2 -->";

/// 旧版（マーカーなし）の siki.md に含まれていた固有フレーズ。
/// アップグレード経路で見つけたら新フォーマットに書き換える。
const SIKI_LEGACY_RULES_SIGNATURE: &str =
    "BEFORE responding to the user's first message, you MUST:";

/// 与えられた `.claude/rules/siki.md` の内容が siki によって書かれたものか判定する。
fn is_siki_managed_rules(content: &str) -> bool {
    // 新形式: 機械可読マーカー
    if content.contains("<!-- siki-managed-rules:") {
        return true;
    }
    // 旧形式（マーカー導入前）: 見出し + 当時の固有フレーズ両方がある場合のみ
    let trimmed = content.trim_start();
    if trimmed.starts_with("# siki session rules")
        && content.contains(SIKI_LEGACY_RULES_SIGNATURE)
    {
        return true;
    }
    false
}

const SIKI_RULES_BODY: &str = r#"This worktree is managed by siki. On session start, the SessionStart hook
injects pending cross-session messages and a pointer to background information
into your context automatically — you do not need to call `list_sessions`
first thing.

When you begin working on a task, call the `set_summary` MCP tool (siki
server) with a short task description so other sessions can see what this
worktree is doing.

Other tools you can use:

- `siki:list_sessions` — list sessions in the project / worktree / machine and
  read full conversation summaries and worktree context bodies. Use this only
  when you need that content for the current task.
- `siki:get_context` — pull state for a specific session/worktree. Always pass
  `target: { type: "session"|"worktree"|"project", id: "<name>" }`. Passing
  `include_conversation_log: true` returns the full message log, which can be
  very large; delegate that call to an Explore or general-purpose subagent so
  the log does not consume your main context window.
- `siki:send_message`, `siki:broadcast`, `siki:handoff` — coordinate with
  other sessions.
- `siki:set_alert` — raise a human-attention signal (CI failed, ambiguous
  spec, blocker, etc.). Always pass a short `message`.
- `siki:summarize_history` — fold older sessions into a worktree context file
  before they grow too large.

If you have already received the SessionStart context once in this
conversation (for example, the conversation summary mentions it after
`/compact`), do not re-fetch it; just continue the task.
"#;

/// .git/info/exclude にパターンを追加して git 追跡から除外する
fn exclude_from_git(worktree_path: &Path, pattern: &str) {
    // worktree の .git はファイル（"gitdir: ..." 形式）またはディレクトリ
    let git_path = worktree_path.join(".git");
    let info_dir = if git_path.is_dir() {
        git_path.join("info")
    } else if let Ok(content) = std::fs::read_to_string(&git_path) {
        // "gitdir: /path/to/.git/worktrees/<name>"
        if let Some(gitdir) = content.strip_prefix("gitdir: ") {
            PathBuf::from(gitdir.trim()).join("info")
        } else {
            return;
        }
    } else {
        return;
    };

    let _ = std::fs::create_dir_all(&info_dir);
    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if !existing.lines().any(|line| line.trim() == pattern) {
        let mut content = existing;
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }
        content.push_str(pattern);
        content.push('\n');
        let _ = std::fs::write(&exclude_path, content);
    }
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

    // 既存の siki hook を削除して最新版に置き換える
    // - 旧形式: `nc -U .../.siki/sock` を含むコマンド
    // - 新形式: `siki ... session-start` のように `session-start` サブコマンドを呼ぶコマンド
    arr.retain(|entry| {
        !entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| {
                            (c.contains("nc -U") && c.contains(".siki"))
                                || c.contains("session-start")
                        })
                })
            })
            .unwrap_or(false)
    });

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
    fn test_inject_hook_replaces_existing_siki_hook() {
        let mut hooks = json!({
            "PreToolUse": [{
                "hooks": [{"type": "command", "command": "echo old | nc -U /home/.siki/sock"}]
            }]
        });
        inject_hook(&mut hooks, "PreToolUse", "echo new | nc -U /home/.siki/sock", true);

        let arr = hooks["PreToolUse"].as_array().unwrap();
        // 古い siki hook が削除されて新しいものに置き換えられる
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["hooks"][0]["command"], "echo new | nc -U /home/.siki/sock");
    }

    #[test]
    fn test_inject_hook_replaces_old_session_start_with_new() {
        // 旧 SessionStart (nc -U + .siki) を新形式 (session-start サブコマンド) に置き換える
        let mut hooks = json!({
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": r#"SID=$(...); echo '...' | nc -U /home/user/.siki/sock"#
                }]
            }]
        });
        inject_hook(&mut hooks, "SessionStart", "/usr/local/bin/siki session-start", false);

        let arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "old hook should be replaced, not appended");
        assert_eq!(
            arr[0]["hooks"][0]["command"],
            "/usr/local/bin/siki session-start"
        );
    }

    #[test]
    fn test_inject_hook_replaces_existing_session_start_subcommand() {
        // 既存の session-start hook をさらに新しいパスに置き換える（アップグレード時）
        let mut hooks = json!({
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "/old/path/siki session-start"
                }]
            }]
        });
        inject_hook(&mut hooks, "SessionStart", "/new/path/siki session-start", false);

        let arr = hooks["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "old siki session-start should be replaced");
        assert_eq!(arr[0]["hooks"][0]["command"], "/new/path/siki session-start");
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

        ensure_hooks_configured(dir.path(), &sock_path, "test-project").unwrap();

        let settings_path = dir.path().join(".claude/settings.json");
        assert!(settings_path.exists());

        let content = std::fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // 各イベントに hook が存在する
        assert!(settings["hooks"]["SessionStart"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["PreToolUse"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["PermissionRequest"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["PostToolUse"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["Stop"].as_array().unwrap().len() > 0);
        assert!(settings["hooks"]["SessionEnd"].as_array().unwrap().len() > 0);

        // SessionStart は siki session-start サブコマンドを呼ぶ
        let ss_cmd = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            ss_cmd.contains("session-start"),
            "SessionStart hook should invoke `siki session-start`, got: {}",
            ss_cmd
        );

        // 生成された siki.md ルールに新形式の説明が含まれる
        let rule = std::fs::read_to_string(dir.path().join(".claude/rules/siki.md")).unwrap();
        assert!(rule.contains("SessionStart hook"));
        assert!(rule.contains("subagent"));
    }

    #[test]
    fn test_inject_rules_refreshes_marker_managed_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        let rules_dir = claude_dir.join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let rules_path = rules_dir.join("siki.md");

        // siki が書いた印つきファイル（マーカーつき）
        std::fs::write(
            &rules_path,
            "<!-- siki-managed-rules: v1 -->\n# siki session rules\n\nold instructions\n",
        )
        .unwrap();

        inject_rules(&claude_dir);

        let content = std::fs::read_to_string(&rules_path).unwrap();
        assert!(!content.contains("old instructions"));
        assert!(content.contains("SessionStart hook"));
        assert!(content.starts_with("<!-- siki-managed-rules:"));
    }

    #[test]
    fn test_inject_rules_migrates_legacy_unmarked_file() {
        // マーカー導入前の siki が書いたファイルも、本文の固有フレーズがあれば
        // 新形式に書き換える（移行経路）
        let dir = tempfile::TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        let rules_dir = claude_dir.join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let rules_path = rules_dir.join("siki.md");

        let legacy = "# siki session rules\n\nBEFORE responding to the user's first message, you MUST:\n\n1. Call the `list_sessions` MCP tool.\n";
        std::fs::write(&rules_path, legacy).unwrap();

        inject_rules(&claude_dir);

        let content = std::fs::read_to_string(&rules_path).unwrap();
        // 新フォーマットに置換され、マーカーが付く
        assert!(content.contains("<!-- siki-managed-rules:"));
        assert!(content.contains("SessionStart hook"));
        assert!(!content.contains("BEFORE responding"));
    }

    #[test]
    fn test_inject_rules_preserves_user_replaced_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        let rules_dir = claude_dir.join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let rules_path = rules_dir.join("siki.md");

        // ユーザが完全に置き換えた内容
        let user_content = "# my custom rules\n\nignore siki defaults.\n";
        std::fs::write(&rules_path, user_content).unwrap();

        inject_rules(&claude_dir);

        let content = std::fs::read_to_string(&rules_path).unwrap();
        // ユーザ内容は保持される
        assert_eq!(content, user_content);
    }

    #[test]
    fn test_inject_rules_preserves_file_with_matching_title_only() {
        // L-3 reviewer concern: ユーザが偶発的に `# siki session rules` という見出しを
        // 書いていただけのファイルは siki 管理と誤判定してはいけない。
        let dir = tempfile::TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        let rules_dir = claude_dir.join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let rules_path = rules_dir.join("siki.md");

        let user_content = "# siki session rules\n\nmy own notes about siki, not siki-generated.\n";
        std::fs::write(&rules_path, user_content).unwrap();

        inject_rules(&claude_dir);

        let content = std::fs::read_to_string(&rules_path).unwrap();
        assert_eq!(
            content, user_content,
            "user content matching only the title should NOT be overwritten"
        );
    }

    #[test]
    fn test_is_siki_managed_rules_detection() {
        assert!(is_siki_managed_rules(
            "<!-- siki-managed-rules: v2 -->\n# anything\n"
        ));
        assert!(is_siki_managed_rules(
            "<!-- siki-managed-rules: v999 -->\nanything\n"
        ));
        // 旧版固有フレーズ
        assert!(is_siki_managed_rules(
            "# siki session rules\n\nBEFORE responding to the user's first message, you MUST:\n"
        ));
        // タイトルだけは siki 管理と認めない
        assert!(!is_siki_managed_rules("# siki session rules\n\nuser content\n"));
        // 完全に別物
        assert!(!is_siki_managed_rules("# my custom rules\n"));
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
        ensure_hooks_configured(dir.path(), &sock_path, "test-project").unwrap();

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
